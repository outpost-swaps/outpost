//! Witness nodes: a second opinion on Alice's Monero lock.
//!
//! Bob releases his redeem signature once a Monero node says Alice's lock exists. A node that lies
//! (or whose operator is also the maker) could make him release his Bitcoin for nothing. A witness
//! is another ordinary Monero node asked the same questions, and every witness has to agree with
//! the main node before the signature is released: does the transaction pay the shared address the
//! agreed amount in an output that can be spent, and has it reached the confirmation target.
//!
//! Nothing secret is sent to a witness: the keys stay local and the node only serves chain data, as
//! the main node does. Witnesses are reached through the same Tor proxy and must be onion or https
//! nodes; the policy for choosing them is on the Java side.
//!
//! The idea and its failure rules come from Ashigaru Desktop's `monero-witness-nodes` patch
//! (GPL-3.0): an unreachable witness is never skipped.

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

/// How long one witness may take to answer one question.
pub const WITNESS_ANSWER_TIMEOUT: Duration = Duration::from_secs(90);
/// How long one witness may take to prove it is usable before a swap starts.
pub const WITNESS_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(60);
/// How often the confirmation wait asks again.
pub const WITNESS_POLL_INTERVAL: Duration = Duration::from_secs(20);
/// How long the confirmation wait lasts before giving up, so the caller's retry (and the cancel
/// timelock) takes over instead of the swap hanging on a lagging witness.
pub const WITNESS_CONFIRMATION_DEADLINE: Duration = Duration::from_secs(30 * 60);

/// One node that can be asked about the lock.
#[async_trait]
pub trait LockWitness: Send + Sync {
    fn name(&self) -> String;

    /// Whether this node sees the lock pay the shared address at least the agreed amount, in an
    /// output that can be spent. An error means the node could not answer.
    async fn lock_is_valid(
        &self,
        tx_hash: &str,
        public_spend_key: &[u8; 32],
        private_view_key: &[u8],
        amount_pico: u64,
    ) -> Result<bool>;

    /// The transaction's confirmations according to this node, `None` while the node does not know
    /// it or has not seen it mined.
    async fn confirmations(&self, tx_hash: &str) -> Result<Option<u64>>;

    /// Whether this node can be used at all: reachable through Tor, a certificate the app accepts,
    /// and answering. Checked before a swap starts, so a node that cannot answer is replaced then,
    /// not discovered after the Bitcoin is locked.
    async fn preflight(&self) -> Result<()> {
        Ok(())
    }
}

/// The result of [`Witnesses::choose`].
#[derive(Debug)]
pub struct Chosen {
    pub witnesses: Witnesses,
    /// How many of the chosen witnesses passed the pre-flight check.
    pub working: usize,
    pub wanted: usize,
    /// Candidates that did not pass, with the reason (for the log).
    pub failed: Vec<(String, String)>,
}

impl Chosen {
    pub fn is_complete(&self) -> bool {
        self.working >= self.wanted
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum WitnessVerdict {
    /// Every witness agrees.
    Agree,
    /// A witness answered and does not see a valid lock.
    Disagree { witness: String },
    /// A witness could not answer; nothing is concluded and nothing may be released.
    Unreachable { witness: String, reason: String },
}

/// A disagreement always wins; an unreachable witness only matters when nobody disagreed.
pub fn combine_verdicts(answers: Vec<(String, Result<bool>)>) -> WitnessVerdict {
    let mut unreachable = None;
    for (witness, answer) in answers {
        match answer {
            Ok(true) => {}
            Ok(false) => return WitnessVerdict::Disagree { witness },
            Err(error) => {
                unreachable.get_or_insert((witness, format!("{error:#}")));
            }
        }
    }
    match unreachable {
        Some((witness, reason)) => WitnessVerdict::Unreachable { witness, reason },
        None => WitnessVerdict::Agree,
    }
}

#[derive(Clone)]
pub struct Witnesses {
    members: Vec<Arc<dyn LockWitness>>,
    answer_timeout: Duration,
}

impl std::fmt::Debug for Witnesses {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Witnesses").field("members", &self.names()).finish()
    }
}

impl Witnesses {
    pub fn new(members: Vec<Arc<dyn LockWitness>>) -> Self {
        Self {
            members,
            answer_timeout: WITNESS_ANSWER_TIMEOUT,
        }
    }

    /// Witnesses that connect to each URL through `proxy` on first use (and again after a failure).
    pub fn from_urls(urls: &[String], proxy: &str) -> Self {
        Self::new(
            urls.iter()
                .map(|url| Arc::new(RemoteWitness::new(url.clone(), proxy.to_owned())) as Arc<dyn LockWitness>)
                .collect(),
        )
    }

    /// Checks every candidate at once and keeps `wanted` of them: the ones that passed, in the
    /// order given, then (only if too few passed) the ones that did not. A node that did not pass
    /// is kept as a last resort rather than dropped, because dropping it would weaken the check;
    /// it then makes the verification wait until it can answer (an unreachable witness is never
    /// skipped). The caller decides whether too few working witnesses is fatal.
    pub async fn choose(candidates: Vec<Arc<dyn LockWitness>>, wanted: usize, timeout: Duration) -> Chosen {
        let outcomes = futures::future::join_all(candidates.iter().map(|candidate| async move {
            match tokio::time::timeout(timeout, candidate.preflight()).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(format!("{error:#}")),
                Err(_) => Err(format!("no answer within {timeout:?}")),
            }
        }))
        .await;
        let mut working = Vec::new();
        let mut not_working = Vec::new();
        let mut failed = Vec::new();
        for (candidate, outcome) in candidates.into_iter().zip(outcomes) {
            match outcome {
                Ok(()) => working.push(candidate),
                Err(reason) => {
                    failed.push((candidate.name(), reason));
                    not_working.push(candidate);
                }
            }
        }
        let working_count = working.len().min(wanted);
        let members = working.into_iter().chain(not_working).take(wanted).collect();
        Chosen {
            witnesses: Witnesses::new(members),
            working: working_count,
            wanted,
            failed,
        }
    }

    pub fn with_answer_timeout(mut self, timeout: Duration) -> Self {
        self.answer_timeout = timeout;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn names(&self) -> Vec<String> {
        self.members.iter().map(|member| member.name()).collect()
    }

    /// Asks every witness at once whether the lock is valid.
    pub async fn verify_lock(
        &self,
        tx_hash: &str,
        public_spend_key: &[u8; 32],
        private_view_key: &[u8],
        amount_pico: u64,
    ) -> WitnessVerdict {
        let answers = futures::future::join_all(self.members.iter().map(|member| async move {
            let answer = match tokio::time::timeout(
                self.answer_timeout,
                member.lock_is_valid(tx_hash, public_spend_key, private_view_key, amount_pico),
            )
            .await
            {
                Ok(answer) => answer,
                Err(_) => Err(anyhow::anyhow!("no answer within {:?}", self.answer_timeout)),
            };
            (member.name(), answer)
        }))
        .await;
        combine_verdicts(answers)
    }

    /// Waits until every witness also sees `target` confirmations, or gives up at `deadline`.
    pub async fn wait_confirmed(
        &self,
        tx_hash: &str,
        target: u64,
        poll: Duration,
        deadline: Duration,
    ) -> Result<()> {
        let give_up = tokio::time::Instant::now() + deadline;
        loop {
            let seen = futures::future::join_all(self.members.iter().map(|member| async move {
                let confirmations = tokio::time::timeout(self.answer_timeout, member.confirmations(tx_hash))
                    .await
                    .ok()
                    .and_then(|answer| answer.ok())
                    .flatten();
                (member.name(), confirmations)
            }))
            .await;
            let behind: Vec<String> = seen
                .iter()
                .filter(|(_, confirmations)| confirmations.is_none_or(|c| c < target))
                .map(|(name, confirmations)| match confirmations {
                    Some(c) => format!("{name} ({c}/{target})"),
                    None => format!("{name} (not seen)"),
                })
                .collect();
            if behind.is_empty() {
                return Ok(());
            }
            if tokio::time::Instant::now() + poll >= give_up {
                anyhow::bail!(
                    "witness node(s) have not seen the Monero lock confirmed within {:?}: {}",
                    deadline,
                    behind.join(", ")
                );
            }
            tokio::time::sleep(poll).await;
        }
    }
}

/// Confirmations from a chain tip and the height of the block holding the transaction.
pub fn confirmations_at(tip: u64, block_height: u64) -> u64 {
    tip.saturating_sub(block_height).saturating_add(1)
}

/// A witness backed by an ordinary daemon, connected through Tor on first use.
pub struct RemoteWitness {
    url: String,
    proxy: String,
    daemon: tokio::sync::OnceCell<super::ng::Daemon>,
}

impl RemoteWitness {
    pub fn new(url: String, proxy: String) -> Self {
        Self {
            url,
            proxy,
            daemon: tokio::sync::OnceCell::new(),
        }
    }

    async fn daemon(&self) -> Result<&super::ng::Daemon> {
        self.daemon
            .get_or_try_init(|| async {
                super::ng::Daemon::connect(&self.url, Some(&self.proxy), None)
                    .await
                    .with_context(|| format!("could not reach witness node {}", self.url))
            })
            .await
    }
}

#[async_trait]
impl LockWitness for RemoteWitness {
    fn name(&self) -> String {
        self.url.clone()
    }

    async fn lock_is_valid(
        &self,
        tx_hash: &str,
        public_spend_key: &[u8; 32],
        private_view_key: &[u8],
        amount_pico: u64,
    ) -> Result<bool> {
        let daemon = self.daemon().await?;
        if !daemon
            .verify_protocol_transfer(tx_hash, public_spend_key, private_view_key, amount_pico)
            .await?
        {
            return Ok(false);
        }
        daemon
            .has_spendable_lock_output(tx_hash, public_spend_key, private_view_key, amount_pico)
            .await
    }

    async fn preflight(&self) -> Result<()> {
        // Connecting and asking for the chain height needs the node, Tor and the node's
        // certificate (checked strictly, like every other connection) to all work.
        self.daemon().await?.chain_height().await.map(|_| ())
    }

    async fn confirmations(&self, tx_hash: &str) -> Result<Option<u64>> {
        let daemon = self.daemon().await?;
        match daemon.tx_status(tx_hash).await? {
            super::ng::TxStatus::Mined(height) => {
                Ok(Some(confirmations_at(daemon.chain_height().await?, height)))
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    enum Behaviour {
        Valid,
        Invalid,
        Down,
        Hangs,
    }

    struct Fake {
        name: &'static str,
        behaviour: Behaviour,
        /// confirmations reported on successive calls (the last one repeats)
        confirmations: Vec<Option<u64>>,
        calls: AtomicU64,
    }

    impl Fake {
        fn new(name: &'static str, behaviour: Behaviour) -> Arc<dyn LockWitness> {
            Arc::new(Self { name, behaviour, confirmations: vec![Some(10)], calls: AtomicU64::new(0) })
        }

        fn with_confirmations(name: &'static str, confirmations: Vec<Option<u64>>) -> Arc<dyn LockWitness> {
            Arc::new(Self { name, behaviour: Behaviour::Valid, confirmations, calls: AtomicU64::new(0) })
        }
    }

    #[async_trait]
    impl LockWitness for Fake {
        fn name(&self) -> String {
            self.name.to_owned()
        }

        async fn lock_is_valid(&self, _: &str, _: &[u8; 32], _: &[u8], _: u64) -> Result<bool> {
            match self.behaviour {
                Behaviour::Valid => Ok(true),
                Behaviour::Invalid => Ok(false),
                Behaviour::Down => anyhow::bail!("connection refused"),
                Behaviour::Hangs => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            }
        }

        async fn preflight(&self) -> Result<()> {
            match self.behaviour {
                Behaviour::Valid | Behaviour::Invalid => Ok(()),
                Behaviour::Down => anyhow::bail!("certificate rejected"),
                Behaviour::Hangs => {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            }
        }

        async fn confirmations(&self, _: &str) -> Result<Option<u64>> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) as usize;
            Ok(self.confirmations[call.min(self.confirmations.len() - 1)])
        }
    }

    fn verdict(members: Vec<Arc<dyn LockWitness>>) -> WitnessVerdict {
        let witnesses = Witnesses::new(members).with_answer_timeout(Duration::from_millis(100));
        futures::executor::block_on(async {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(witnesses.verify_lock("tx", &[0; 32], &[0; 32], 1))
        })
    }

    #[test]
    fn all_witnesses_agreeing_is_agreement() {
        assert_eq!(verdict(vec![Fake::new("a", Behaviour::Valid), Fake::new("b", Behaviour::Valid)]), WitnessVerdict::Agree);
    }

    // The attack this closes: the main node says the lock exists, one witness has never seen it.
    #[test]
    fn one_dissenting_witness_blocks_the_release() {
        assert_eq!(
            verdict(vec![Fake::new("a", Behaviour::Valid), Fake::new("b", Behaviour::Invalid)]),
            WitnessVerdict::Disagree { witness: "b".into() }
        );
    }

    #[test]
    fn an_unreachable_witness_is_never_skipped() {
        match verdict(vec![Fake::new("a", Behaviour::Valid), Fake::new("b", Behaviour::Down)]) {
            WitnessVerdict::Unreachable { witness, reason } => {
                assert_eq!(witness, "b");
                assert!(reason.contains("connection refused"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_witness_that_never_answers_counts_as_unreachable() {
        assert!(matches!(
            verdict(vec![Fake::new("a", Behaviour::Valid), Fake::new("slow", Behaviour::Hangs)]),
            WitnessVerdict::Unreachable { witness, .. } if witness == "slow"
        ));
    }

    #[test]
    fn a_disagreement_outranks_an_unreachable_witness() {
        assert!(matches!(
            combine_verdicts(vec![
                ("down".into(), Err(anyhow::anyhow!("down"))),
                ("no".into(), Ok(false)),
                ("yes".into(), Ok(true)),
            ]),
            WitnessVerdict::Disagree { witness } if witness == "no"
        ));
    }

    #[test]
    fn no_witnesses_agree_trivially() {
        assert_eq!(combine_verdicts(vec![]), WitnessVerdict::Agree);
    }

    fn wait(members: Vec<Arc<dyn LockWitness>>, target: u64, deadline_ms: u64) -> Result<()> {
        let witnesses = Witnesses::new(members).with_answer_timeout(Duration::from_millis(100));
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(witnesses.wait_confirmed("tx", target, Duration::from_millis(5), Duration::from_millis(deadline_ms)))
    }

    #[test]
    fn the_wait_ends_when_every_witness_reaches_the_target() {
        wait(
            vec![
                Fake::with_confirmations("a", vec![Some(10)]),
                Fake::with_confirmations("b", vec![None, Some(3), Some(10)]),
            ],
            10,
            2_000,
        )
        .unwrap();
    }

    #[test]
    fn a_lagging_witness_makes_the_wait_fail_and_is_named() {
        let error = wait(
            vec![Fake::with_confirmations("a", vec![Some(10)]), Fake::with_confirmations("stuck", vec![Some(4)])],
            10,
            60,
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("stuck (4/10)"), "{message}");
        assert!(!message.contains("a ("), "{message}");
    }

    #[test]
    fn a_witness_that_has_not_seen_the_transaction_is_waited_for() {
        assert!(wait(vec![Fake::with_confirmations("late", vec![None])], 1, 40).is_err());
    }

    #[test]
    fn witnesses_built_from_urls_keep_their_order_and_names() {
        let urls = vec!["http://aaa.onion:18081".to_owned(), "https://node.example.org:18081".to_owned()];
        let witnesses = Witnesses::from_urls(&urls, "127.0.0.1:9050");
        assert_eq!(witnesses.names(), urls);
        assert!(!witnesses.is_empty());
        assert!(Witnesses::from_urls(&[], "127.0.0.1:9050").is_empty());
    }

    #[test]
    fn confirmations_count_the_block_itself() {
        assert_eq!(confirmations_at(100, 100), 1);
        assert_eq!(confirmations_at(109, 100), 10);
        assert_eq!(confirmations_at(99, 100), 1, "a node behind the block still counts it once");
    }

    fn choose(candidates: Vec<Arc<dyn LockWitness>>, wanted: usize) -> Chosen {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(Witnesses::choose(candidates, wanted, Duration::from_millis(100)))
    }

    #[test]
    fn working_candidates_are_kept_in_the_order_given() {
        let chosen = choose(
            vec![Fake::new("a", Behaviour::Valid), Fake::new("b", Behaviour::Valid), Fake::new("c", Behaviour::Valid)],
            2,
        );
        assert_eq!(chosen.witnesses.names(), vec!["a", "b"]);
        assert!(chosen.is_complete());
        assert!(chosen.failed.is_empty());
    }

    #[test]
    fn a_spare_replaces_a_candidate_that_failed_the_preflight() {
        let chosen = choose(
            vec![Fake::new("a", Behaviour::Down), Fake::new("b", Behaviour::Valid), Fake::new("c", Behaviour::Valid)],
            2,
        );
        assert_eq!(chosen.witnesses.names(), vec!["b", "c"]);
        assert!(chosen.is_complete());
        assert_eq!(chosen.failed.len(), 1);
        assert_eq!(chosen.failed[0].0, "a");
        assert!(chosen.failed[0].1.contains("certificate rejected"));
    }

    #[test]
    fn a_candidate_that_never_answers_counts_as_failed() {
        let chosen = choose(vec![Fake::new("a", Behaviour::Hangs), Fake::new("b", Behaviour::Valid)], 1);
        assert_eq!(chosen.witnesses.names(), vec!["b"]);
        assert!(chosen.is_complete());
        assert!(chosen.failed[0].1.contains("no answer"));
    }

    #[test]
    fn too_few_working_candidates_is_reported_and_the_failed_ones_fill_the_places() {
        let chosen = choose(
            vec![Fake::new("a", Behaviour::Down), Fake::new("b", Behaviour::Valid), Fake::new("c", Behaviour::Down)],
            2,
        );
        assert_eq!(chosen.working, 1);
        assert!(!chosen.is_complete());
        // the working one first, then a failed one: never fewer places than wanted
        assert_eq!(chosen.witnesses.names(), vec!["b", "a"]);
    }

    #[test]
    fn a_failed_candidate_is_not_silently_dropped_from_the_check() {
        let chosen = choose(vec![Fake::new("a", Behaviour::Down)], 1);
        assert_eq!(chosen.working, 0);
        assert_eq!(chosen.witnesses.names(), vec!["a"]);
        // and the verification then waits for it rather than passing
        let verdict = futures::executor::block_on(async {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(chosen.witnesses.verify_lock("tx", &[0; 32], &[0; 32], 1))
        });
        assert!(matches!(verdict, WitnessVerdict::Unreachable { .. }));
    }
}
