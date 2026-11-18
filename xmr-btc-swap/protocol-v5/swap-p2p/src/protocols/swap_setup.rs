use anyhow::{Context, Result};
use asynchronous_codec::{Bytes, Framed};
use futures::{AsyncRead, AsyncWrite, SinkExt, StreamExt};

use libp2p::swarm::Stream;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub mod alice;
pub mod bob;
mod vendor_from_fn;
pub mod verification;

/// Size limit of every swap setup message.
///
/// The largest message carries a cross-curve DLEQ proof of ~112 KB in CBOR. Its size
/// does not depend on user input. 300 KiB is 2.7x that.
pub const BUF_SIZE: usize = 300 * 1024;

pub mod protocol {
    use futures::future;
    use libp2p::core::Endpoint;
    use libp2p::swarm::Stream;
    use void::Void;

    use super::vendor_from_fn::{FromFnUpgrade, from_fn};

    pub fn new() -> SwapSetup {
        from_fn(
            "/comit/xmr/btc/swap_setup/5.0.0",
            Box::new(|socket, _| future::ready(Ok(socket))),
        )
    }

    pub type SwapSetup = FromFnUpgrade<
        &'static str,
        Box<dyn Fn(Stream, Endpoint) -> future::Ready<Result<Stream, Void>> + Send + 'static>,
    >;
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockchainNetwork {
    #[serde(with = "swap_serde::bitcoin::network")]
    pub bitcoin: bitcoin::Network,
    #[serde(with = "swap_serde::monero::network")]
    pub monero: monero_address::Network,
}

/// Bob's last setup message, sent after the user decided whether to lock the Bitcoin.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BobDecision {
    Commit,
    Abort,
}

/// Alice persisted the swap. Bob must not publish the lock transaction without it,
/// because Alice would not know the swap and could not refund it early.
#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct SwapPersisted;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SpotPriceRequest {
    pub btc: bitcoin::Amount,
    pub blockchain_network: BlockchainNetwork,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum SpotPriceResponse {
    Xmr(swap_core::monero::Amount),
    Error(SpotPriceError),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum SwapSetupError {
    #[error("Invalid swap fee proposal: {0}")]
    InvalidFeeProposal(String),
    #[error("Bitcoin lock transaction has too many inputs ({actual}, maximum: {maximum})")]
    TxLockTooManyInputs { actual: u32, maximum: u32 },
    #[error("Input verification failed.")]
    LockInputVerificationFailed,
    #[error("Input verification was inconclusive.")]
    LockInputVerificationInconclusive,
    #[error("Invalid DLEQ proof")]
    InvalidDleqProof,
    #[error("Anti-spam deposit {deposit} is too small; minimum required is {minimum}")]
    AntiSpamDepositTooSmall {
        deposit: bitcoin::Amount,
        minimum: bitcoin::Amount,
    },
    #[error("Anti-spam deposit ratio {ratio} exceeds maximum {max}")]
    AntiSpamDepositRatioTooHigh {
        ratio: rust_decimal::Decimal,
        max: rust_decimal::Decimal,
    },
    #[error("Swap spending path fees ({fees}) exceed local budget ({budget})")]
    SwapFeesExceedBudget {
        fees: bitcoin::Amount,
        budget: bitcoin::Amount,
    },
    #[error("Transaction fee is too low compared to our estimate ({fee} vs {estimate})")]
    TransactionFeeTooLow {
        fee: bitcoin::Amount,
        estimate: bitcoin::Amount,
    },
}

impl SwapSetupError {
    /// Whether the setup may succeed if Bob starts it again.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::LockInputVerificationInconclusive)
    }
}

impl TryFrom<anyhow::Error> for SwapSetupError {
    type Error = anyhow::Error;

    fn try_from(error: anyhow::Error) -> Result<Self> {
        use swap_core::bitcoin::SwapFeeError;
        use swap_machine::common::SetupValidationError;

        // Only send explicit validation failures. Wallet and other local errors
        // retain their context locally and propagate through the retry path.
        if let Some(validation) = error.downcast_ref::<SwapFeeError>() {
            return Ok(match *validation {
                SwapFeeError::RateOutsideLimits { .. } => {
                    Self::InvalidFeeProposal(validation.to_string())
                }
                SwapFeeError::AntiSpamDepositTooSmall { deposit, minimum } => {
                    Self::AntiSpamDepositTooSmall { deposit, minimum }
                }
                SwapFeeError::SwapFeesExceedBudget { fees, budget } => {
                    Self::SwapFeesExceedBudget { fees, budget }
                }
            });
        }
        if let Some(validation) = error.downcast_ref::<SetupValidationError>() {
            return Ok(match *validation {
                SetupValidationError::InvalidDleqProof => Self::InvalidDleqProof,
                SetupValidationError::AntiSpamDepositRatioTooHigh { ratio, max } => {
                    Self::AntiSpamDepositRatioTooHigh { ratio, max }
                }
                SetupValidationError::TransactionFeeTooLow { fee, estimate } => {
                    Self::TransactionFeeTooLow { fee, estimate }
                }
            });
        }
        Err(error)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SpotPriceError {
    NoSwapsAccepted,
    AmountBelowMinimum {
        min: bitcoin::Amount,
        buy: bitcoin::Amount,
    },
    AmountAboveMaximum {
        max: bitcoin::Amount,
        buy: bitcoin::Amount,
    },
    BalanceTooLow {
        buy: bitcoin::Amount,
    },
    BlockchainNetworkMismatch {
        cli: BlockchainNetwork,
        asb: BlockchainNetwork,
    },
    /// To be used for errors that cannot be explained on the CLI side (e.g.
    /// rate update problems on the seller side)
    Other,
}

fn codec() -> unsigned_varint::codec::UviBytes<Bytes> {
    let mut codec = unsigned_varint::codec::UviBytes::<Bytes>::default();
    codec.set_max_len(BUF_SIZE);
    codec
}

/// The swap setup substream. It is framed once for the whole setup, because the
/// framing buffers what it reads and a message read ahead would otherwise be lost.
pub type SetupStream<S = Stream> = Framed<S, unsigned_varint::codec::UviBytes<Bytes>>;

pub fn framed<S>(stream: S) -> SetupStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    Framed::new(stream, codec())
}

pub async fn read_cbor_message<T>(
    stream: &mut SetupStream<impl AsyncRead + AsyncWrite + Unpin>,
) -> Result<Result<T, SwapSetupError>>
where
    T: DeserializeOwned,
{
    let bytes = stream
        .next()
        .await
        .context("Failed to read length-prefixed message from stream")??;

    let mut de = serde_cbor::Deserializer::from_slice(&bytes);
    let message = Result::<T, SwapSetupError>::deserialize(&mut de)
        .context("Failed to deserialize bytes into message using CBOR")?;

    Ok(message)
}

pub async fn write_cbor_message<T>(
    stream: &mut SetupStream<impl AsyncRead + AsyncWrite + Unpin>,
    message: T,
) -> Result<()>
where
    T: Serialize,
{
    let wrapped = Ok::<_, SwapSetupError>(message);
    let bytes =
        serde_cbor::to_vec(&wrapped).context("Failed to serialize message as bytes using CBOR")?;

    stream
        .send(Bytes::from(bytes))
        .await
        .context("Failed to write bytes as length-prefixed message")?;

    Ok(())
}

pub async fn write_cbor_error(
    stream: &mut SetupStream<impl AsyncRead + AsyncWrite + Unpin>,
    error: SwapSetupError,
) -> Result<()> {
    let wrapped = Err::<(), _>(error);
    let bytes =
        serde_cbor::to_vec(&wrapped).context("Failed to serialize error as bytes using CBOR")?;

    stream
        .send(Bytes::from(bytes))
        .await
        .context("Failed to write bytes as length-prefixed message")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::Amount;
    use rust_decimal::Decimal;
    use swap_core::bitcoin::{SwapFeeError, SwapFeeRate};
    use swap_machine::common::SetupValidationError;

    #[test]
    fn validation_errors_keep_their_kind_and_values_on_the_wire() {
        let deposit = Amount::ONE_SAT;
        let minimum = Amount::from_sat(656);
        let ratio = Decimal::new(3, 1);
        let max = Decimal::new(2, 1);
        let fees = Amount::from_sat(100_001);
        let budget = Amount::from_sat(100_000);
        let rate_error = SwapFeeError::RateOutsideLimits {
            rate: SwapFeeRate(999),
            min: SwapFeeRate(1000),
            max: SwapFeeRate(3000),
        };
        let cases: Vec<(anyhow::Error, SwapSetupError)> = vec![
            (
                rate_error.into(),
                SwapSetupError::InvalidFeeProposal(rate_error.to_string()),
            ),
            (
                SetupValidationError::InvalidDleqProof.into(),
                SwapSetupError::InvalidDleqProof,
            ),
            (
                SwapFeeError::AntiSpamDepositTooSmall { deposit, minimum }.into(),
                SwapSetupError::AntiSpamDepositTooSmall { deposit, minimum },
            ),
            (
                SetupValidationError::AntiSpamDepositRatioTooHigh { ratio, max }.into(),
                SwapSetupError::AntiSpamDepositRatioTooHigh { ratio, max },
            ),
            (
                SwapFeeError::SwapFeesExceedBudget { fees, budget }.into(),
                SwapSetupError::SwapFeesExceedBudget { fees, budget },
            ),
            (
                SetupValidationError::TransactionFeeTooLow {
                    fee: deposit,
                    estimate: minimum,
                }
                .into(),
                SwapSetupError::TransactionFeeTooLow {
                    fee: deposit,
                    estimate: minimum,
                },
            ),
        ];
        for (error, expected) in cases {
            let rejection = SwapSetupError::try_from(error.context("local-only context")).unwrap();
            assert_eq!(rejection, expected);
            let bytes = serde_cbor::to_vec(&Err::<(), _>(rejection)).unwrap();
            let decoded: Result<(), SwapSetupError> = serde_cbor::from_slice(&bytes).unwrap();
            assert_eq!(decoded.unwrap_err(), expected);
            assert!(!expected.to_string().contains("local-only context"));
        }
    }

    #[test]
    fn local_errors_are_not_protocol_rejections() {
        let error = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "private Electrum server details",
        ))
        .context("Failed to build lock transaction");
        let local_error = SwapSetupError::try_from(error).unwrap_err();
        assert_eq!(
            local_error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::TimedOut
        );
        assert!(format!("{local_error:#}").contains("private Electrum server details"));
    }

    #[tokio::test]
    async fn messages_sent_back_to_back_are_all_read() {
        let mut sent = framed(futures::io::Cursor::new(Vec::new()));
        write_cbor_message(&mut sent, "message 4").await.unwrap();
        write_cbor_message(&mut sent, BobDecision::Commit)
            .await
            .unwrap();
        let bytes = sent.into_inner().into_inner();

        // Both messages arrive in a single read, so the first read buffers the second one
        let mut received = framed(futures::io::Cursor::new(bytes));
        let message4 = read_cbor_message::<String>(&mut received)
            .await
            .unwrap()
            .unwrap();
        let decision = read_cbor_message::<BobDecision>(&mut received)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(message4, "message 4");
        assert_eq!(decision, BobDecision::Commit);
    }

    /// Wire framing (matrix W-01): an unsigned-varint length, then the CBOR of
    /// `Result<T, SwapSetupError>`. Pinned byte for byte; a different layout would not
    /// interoperate with Eigenwallet's `swap_setup/5.0.0`.
    #[tokio::test]
    async fn a_message_is_a_varint_length_followed_by_its_cbor() {
        let mut sent = framed(futures::io::Cursor::new(Vec::new()));
        write_cbor_message(&mut sent, "message 4").await.unwrap();
        let bytes = sent.into_inner().into_inner();

        let mut expected = vec![
            14, // varint length of the CBOR below
            0xa1, // map of one entry
            0x62, b'O', b'k', // key "Ok" (the `Result::Ok` variant)
            0x69, // text string of 9 bytes
        ];
        expected.extend_from_slice(b"message 4");
        assert_eq!(bytes, expected);
    }

    #[tokio::test]
    async fn a_frame_longer_than_the_limit_is_rejected_on_read() {
        // A complete, valid frame that is too long: only the limit can reject it.
        let cbor = serde_cbor::to_vec(&Ok::<String, SwapSetupError>("x".repeat(BUF_SIZE + 10))).unwrap();
        assert!(cbor.len() > BUF_SIZE);
        let mut length = unsigned_varint::encode::usize_buffer();
        let mut frame = unsigned_varint::encode::usize(cbor.len(), &mut length).to_vec();
        frame.extend_from_slice(&cbor);

        let mut received = framed(futures::io::Cursor::new(frame));
        assert!(read_cbor_message::<String>(&mut received).await.is_err());
    }

    #[tokio::test]
    async fn a_message_larger_than_the_limit_is_not_written() {
        let mut sent = framed(futures::io::Cursor::new(Vec::new()));
        let too_big = "x".repeat(BUF_SIZE + 10);
        assert!(write_cbor_message(&mut sent, too_big).await.is_err());
    }

    #[tokio::test]
    async fn a_message_just_under_the_limit_round_trips() {
        let message = "x".repeat(BUF_SIZE - 64);
        let mut sent = framed(futures::io::Cursor::new(Vec::new()));
        write_cbor_message(&mut sent, message.clone()).await.unwrap();
        let bytes = sent.into_inner().into_inner();

        let mut received = framed(futures::io::Cursor::new(bytes));
        let read = read_cbor_message::<String>(&mut received).await.unwrap().unwrap();
        assert_eq!(read, message);
    }

    #[tokio::test]
    async fn a_truncated_frame_is_an_error_not_a_hang() {
        // says 100 bytes, delivers 10, then the stream ends
        let mut frame = vec![100u8];
        frame.extend_from_slice(&[0xa1; 10]);
        let mut received = framed(futures::io::Cursor::new(frame));
        assert!(read_cbor_message::<String>(&mut received).await.is_err());
    }

    #[tokio::test]
    async fn a_frame_that_is_not_cbor_is_an_error() {
        let mut frame = vec![4u8];
        frame.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        let mut received = framed(futures::io::Cursor::new(frame));
        assert!(read_cbor_message::<String>(&mut received).await.is_err());
    }

    #[test]
    fn cross_curve_dleq_proof_is_below_half_of_the_size_limit() {
        use swap_core::compat::IntoDalekNg;

        let mut rng = rand::thread_rng();
        let secret = swap_core::monero::Scalar::random(&mut rng).into_dalek_ng();
        let (proof, _) = swap_machine::common::CROSS_CURVE_PROOF_SYSTEM.prove(&secret, &mut rng);

        let size = serde_cbor::to_vec(&proof).unwrap().len();
        assert!(2 * size <= BUF_SIZE, "DLEQ proof has {size} bytes");
    }
}

#[cfg(test)]
mod headless_setup_tests {
    use anyhow::Context;
    use super::alice::{
        self, LockInputCheck, LockInputCheckError, LockInputReservation, WalletSnapshot,
    };
    use super::bob::{self, NewSwap};
    use super::verification::LockInputVerifier;
    use bitcoin::Amount;
    use bitcoin_wallet::{TestWalletBuilder, VerifyPsbtPrevouts};
    use std::sync::Arc;
    use std::time::Duration;
    use swap_env::env::{GetConfig, Regtest};
    use swap_machine::common::swap_fee_limits;
    use tokio_util::compat::TokioAsyncReadCompatExt;

    struct NoopPrevouts;

    #[async_trait::async_trait]
    impl VerifyPsbtPrevouts for NoopPrevouts {
        async fn verify_psbt_prevouts(
            &self,
            _: &bitcoin::psbt::Psbt,
            _: &tokio::sync::watch::Receiver<bool>,
        ) -> Result<(), bitcoin_wallet::PsbtPrevoutVerificationError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn setup_over_in_memory_substream_succeeds() -> anyhow::Result<()> {
        let config = Regtest::get_config();
        let btc = Amount::from_sat(500_000);
        let fee_limits = swap_fee_limits(swap_core::bitcoin::SwapFeeRate(5_000), btc)?;

        let alice_wallet = TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
            .build()
            .await;
        let bob_wallet = Arc::new(
            TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
                .build()
                .await,
        );

        let snapshot = WalletSnapshot::new(
            swap_core::monero::Amount::from_pico(1_000_000_000_000),
            alice_wallet.new_address().await?,
            alice_wallet.new_address().await?,
            bitcoin::FeeRate::from_sat_per_vb(1).unwrap(),
            swap_core::bitcoin::SwapFeeRate(5_000),
            swap_core::bitcoin::RedeemFeeMultiplier::try_from(250)?,
            fee_limits,
        );

        let (snapshot_sender, mut snapshot_receiver) = bmrng::channel_with_timeout::<
            Amount,
            (WalletSnapshot, Amount, bool),
        >(1, Duration::from_secs(30));
        tokio::spawn(async move {
            let (_, responder) = snapshot_receiver.recv().await?;
            responder.respond((snapshot, Amount::ZERO, false))?;
            Ok::<_, anyhow::Error>(())
        });

        let (check_sender, mut check_receiver) = bmrng::channel::<
            LockInputCheck,
            Result<LockInputReservation, LockInputCheckError>,
        >(1);
        let (release_sender, _release_receiver) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let (check, responder) = check_receiver.recv().await?;
            let outpoints = check
                .psbt
                .unsigned_tx
                .input
                .iter()
                .map(|input| input.previous_output)
                .collect();
            responder.respond(Ok(LockInputReservation::new(outpoints, release_sender)))?;
            Ok::<_, anyhow::Error>(())
        });

        let verifier = LockInputVerifier::spawn(Arc::new(NoopPrevouts));
        let rate = swap_feed::Rate::new(
            Amount::from_btc(0.01)?,
            rust_decimal::Decimal::ZERO,
        );

        let (alice_io, bob_io) = tokio::io::duplex(8 * 1024 * 1024);
        let alice_stream = alice_io.compat();
        let bob_stream = bob_io.compat();

        let bob_swap = NewSwap {
            swap_id: uuid::Uuid::new_v4(),
            btc,
            tx_lock_fee: Amount::from_sat(1_000),
            fee_limits,
            bitcoin_refund_address: bob_wallet.new_address().await?,
            quoted_price: Amount::from_btc(0.01)?,
        };

        let bob_future =
            bob::run_swap_setup(bob_stream, bob_swap, config, bob_wallet.clone());
        let alice_future = alice::run_swap_setup(
            alice_stream,
            snapshot_sender,
            check_sender,
            verifier,
            libp2p::PeerId::random(),
            false,
            config,
            Amount::from_sat(1),
            Amount::ONE_BTC,
            Ok(rate),
        );

        let (bob_result, alice_result) = tokio::join!(bob_future, alice_future);
        let (bob_state2, _bob_stream) = bob_result?;
        let (_swap_id, alice_state3, _reservation, _alice_stream) = alice_result?;

        let (bob_state3, _tx_lock) = bob_state2.clone().lock_btc().await?;
        assert_eq!(bob_state3.tx_lock_id(), alice_state3.tx_lock.txid());

        Ok(())
    }

    #[tokio::test]
    async fn setup_over_libp2p_swarm_commits() -> anyhow::Result<()> {
        use crate::test::{SwarmExt, new_swarm};
        use futures::StreamExt;
        use libp2p::swarm::SwarmEvent;
        use swap_feed::FixedRate;

        let config = Regtest::get_config();
        let btc = Amount::from_sat(500_000);
        let fee_limits = swap_fee_limits(swap_core::bitcoin::SwapFeeRate(5_000), btc)?;
        let alice_wallet = TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
            .build()
            .await;
        let bob_wallet = Arc::new(
            TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
                .build()
                .await,
        );
        let snapshot = WalletSnapshot::new(
            swap_core::monero::Amount::from_pico(1_000_000_000_000),
            alice_wallet.new_address().await?,
            alice_wallet.new_address().await?,
            bitcoin::FeeRate::from_sat_per_vb(1).unwrap(),
            swap_core::bitcoin::SwapFeeRate(5_000),
            swap_core::bitcoin::RedeemFeeMultiplier::try_from(250)?,
            fee_limits,
        );

        let verifier = LockInputVerifier::spawn(Arc::new(NoopPrevouts));
        let mut alice = new_swarm(|_| {
            alice::Behaviour::new(
                Amount::from_sat(1),
                Amount::ONE_BTC,
                config,
                FixedRate::default(),
                false,
                verifier,
            )
        });
        let mut bob = new_swarm(|_| bob::Behaviour::new(config, bob_wallet.clone()));

        let alice_peer_id = *alice.local_peer_id();
        alice.listen_on_random_memory_address().await;
        bob.block_on_connection(&mut alice).await;

        bob.behaviour_mut().queue_new_swap(
            alice_peer_id,
            NewSwap {
                swap_id: uuid::Uuid::new_v4(),
                btc,
                tx_lock_fee: Amount::from_sat(1_000),
                fee_limits,
                bitcoin_refund_address: bob_wallet.new_address().await?,
                quoted_price: Amount::from_btc(0.01)?,
            },
        );

        let (alice_txid_tx, alice_txid_rx) = tokio::sync::oneshot::channel();
        let (commit_tx, mut commit_rx) = tokio::sync::oneshot::channel();
        let mut bob_txid = None;
        let mut alice_txid_tx = Some(alice_txid_tx);
        let mut commit_tx = Some(commit_tx);

        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                tokio::select! {
                    event = alice.select_next_some() => {
                        if let SwarmEvent::Behaviour(alice::OutEvent::Initiated {
                            mut send_wallet_snapshot,
                            mut check_lock_inputs,
                            mut register_swap,
                            ..
                        }) = event
                        {
                            let snapshot = snapshot.clone();
                            let alice_txid_tx = alice_txid_tx.take();
                            tokio::spawn(async move {
                                let (_, responder) = send_wallet_snapshot.recv().await?;
                                responder.respond((snapshot, Amount::ZERO, false))?;

                                let (check, responder) = check_lock_inputs.recv().await?;
                                let outpoints = check
                                    .psbt
                                    .unsigned_tx
                                    .input
                                    .iter()
                                    .map(|input| input.previous_output)
                                    .collect();
                                let (release_sender, _release_receiver) =
                                    tokio::sync::mpsc::unbounded_channel();
                                responder.respond(Ok(LockInputReservation::new(
                                    outpoints,
                                    release_sender,
                                )))?;

                                let (register, responder) = register_swap.recv().await?;
                                if let Some(alice_txid_tx) = alice_txid_tx {
                                    let _ = alice_txid_tx.send(register.state3.tx_lock.txid());
                                }
                                responder.respond(Ok(()))?;
                                anyhow::Ok(())
                            });
                        }
                    }
                    event = bob.select_next_some() => {
                        if let SwarmEvent::Behaviour(result) = event {
                            let event: crate::out_event::bob::OutEvent = result.into();
                            if let crate::out_event::bob::OutEvent::SwapSetupCompleted {
                                result, ..
                            } = event
                            {
                                let (state2, pending) = (*result)?;
                                bob_txid = Some(state2.clone().lock_btc().await?.0.tx_lock_id());
                                if let Some(commit_tx) = commit_tx.take() {
                                    tokio::spawn(async move {
                                        let result = pending.commit().await;
                                        let _ = commit_tx.send(result);
                                    });
                                }
                            }
                        }
                    }
                    committed = &mut commit_rx => {
                        committed??;
                        break;
                    }
                }
            }
            anyhow::Ok(())
        })
        .await
        .context("libp2p swap setup timed out")??;

        let alice_txid = alice_txid_rx
            .await
            .context("Alice did not register the swap")?;
        assert_eq!(Some(alice_txid), bob_txid);

        Ok(())
    }

    // Audit 2026-11-10, C-1: the maker names the XMR amount in setup, after its quote. A maker
    // that quotes fairly and then offers less must be refused before anything is signed or
    // reserved. These run the real buyer and maker setup over an in-memory stream.

    struct SetupOutcome {
        bob: Result<swap_machine::bob::State2, String>,
        bob_rejected: bool,
        alice_finished: bool,
        lock_inputs_checked: bool,
    }

    /// Runs one setup in which the maker prices at `maker_rate` and the buyer acts on a quote of
    /// `quoted_price`, both in BTC per XMR.
    async fn setup_with_rates(maker_rate: Amount, quoted_price: Amount) -> anyhow::Result<SetupOutcome> {
        use std::sync::atomic::{AtomicBool, Ordering};

        let config = Regtest::get_config();
        let btc = Amount::from_sat(500_000);
        let fee_limits = swap_fee_limits(swap_core::bitcoin::SwapFeeRate(5_000), btc)?;
        let alice_wallet = TestWalletBuilder::new(Amount::ONE_BTC.to_sat()).build().await;
        let bob_wallet = Arc::new(TestWalletBuilder::new(Amount::ONE_BTC.to_sat()).build().await);

        let snapshot = WalletSnapshot::new(
            // enough XMR for any rate used below
            swap_core::monero::Amount::from_pico(1_000_000_000_000_000),
            alice_wallet.new_address().await?,
            alice_wallet.new_address().await?,
            bitcoin::FeeRate::from_sat_per_vb(1).unwrap(),
            swap_core::bitcoin::SwapFeeRate(5_000),
            swap_core::bitcoin::RedeemFeeMultiplier::try_from(250)?,
            fee_limits,
        );
        let (snapshot_sender, mut snapshot_receiver) = bmrng::channel_with_timeout::<
            Amount,
            (WalletSnapshot, Amount, bool),
        >(1, Duration::from_secs(30));
        tokio::spawn(async move {
            let (_, responder) = snapshot_receiver.recv().await?;
            responder.respond((snapshot, Amount::ZERO, false))?;
            Ok::<_, anyhow::Error>(())
        });

        let lock_inputs_checked = Arc::new(AtomicBool::new(false));
        let checked = lock_inputs_checked.clone();
        let (check_sender, mut check_receiver) = bmrng::channel::<
            LockInputCheck,
            Result<LockInputReservation, LockInputCheckError>,
        >(1);
        let (release_sender, _release_receiver) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            let (check, responder) = check_receiver.recv().await?;
            checked.store(true, Ordering::SeqCst);
            let outpoints = check
                .psbt
                .unsigned_tx
                .input
                .iter()
                .map(|input| input.previous_output)
                .collect();
            responder.respond(Ok(LockInputReservation::new(outpoints, release_sender)))?;
            Ok::<_, anyhow::Error>(())
        });

        let verifier = LockInputVerifier::spawn(Arc::new(NoopPrevouts));
        let rate = swap_feed::Rate::new(maker_rate, rust_decimal::Decimal::ZERO);
        let (alice_io, bob_io) = tokio::io::duplex(8 * 1024 * 1024);

        let bob_swap = NewSwap {
            swap_id: uuid::Uuid::new_v4(),
            btc,
            tx_lock_fee: Amount::from_sat(1_000),
            fee_limits,
            bitcoin_refund_address: bob_wallet.new_address().await?,
            quoted_price,
        };
        let bob_future = bob::run_swap_setup(bob_io.compat(), bob_swap, config, bob_wallet.clone());
        let alice_future = alice::run_swap_setup(
            alice_io.compat(),
            snapshot_sender,
            check_sender,
            verifier,
            libp2p::PeerId::random(),
            false,
            config,
            Amount::from_sat(1),
            Amount::ONE_BTC,
            Ok(rate),
        );

        let (bob_result, alice_result) = tokio::time::timeout(
            Duration::from_secs(60),
            async { tokio::join!(bob_future, alice_future) },
        )
        .await
        .context("setup timed out")?;

        let bob_rejected = matches!(bob_result, Err(bob::SetupError::Rejected(_)));
        Ok(SetupOutcome {
            bob: bob_result.map(|(state2, _)| state2).map_err(|e| e.to_string()),
            bob_rejected,
            alice_finished: alice_result.is_ok(),
            lock_inputs_checked: lock_inputs_checked.load(Ordering::SeqCst),
        })
    }

    fn btc(value: f64) -> Amount {
        Amount::from_btc(value).unwrap()
    }

    #[tokio::test]
    async fn an_honest_maker_at_the_quoted_price_is_accepted() -> anyhow::Result<()> {
        let outcome = setup_with_rates(btc(0.01), btc(0.01)).await?;
        let state2 = outcome.bob.map_err(anyhow::Error::msg)?;
        // 0.005 BTC at 0.01 BTC/XMR
        assert_eq!(state2.xmr.as_pico(), 500_000_000_000);
        assert!(outcome.alice_finished);
        Ok(())
    }

    #[tokio::test]
    async fn a_maker_cheaper_than_its_quote_is_accepted() -> anyhow::Result<()> {
        let outcome = setup_with_rates(btc(0.009), btc(0.01)).await?;
        let state2 = outcome.bob.map_err(anyhow::Error::msg)?;
        assert!(state2.xmr.as_pico() > 500_000_000_000);
        Ok(())
    }

    #[tokio::test]
    async fn a_rate_move_inside_the_tolerance_is_accepted() -> anyhow::Result<()> {
        // 0.5% dearer than quoted
        let outcome = setup_with_rates(btc(0.01005), btc(0.01)).await?;
        assert!(outcome.bob.is_ok(), "{:?}", outcome.bob.err());
        Ok(())
    }

    #[tokio::test]
    async fn a_rate_move_beyond_the_tolerance_is_refused() -> anyhow::Result<()> {
        // 2% dearer than quoted
        let outcome = setup_with_rates(btc(0.0102), btc(0.01)).await?;
        assert!(outcome.bob_rejected, "{:?}", outcome.bob.as_ref().err());
        assert!(!outcome.lock_inputs_checked, "nothing may be reserved or signed");
        Ok(())
    }

    #[tokio::test]
    async fn a_maker_offering_far_less_than_quoted_is_refused_before_anything_is_locked(
    ) -> anyhow::Result<()> {
        // the quote says 0.01 BTC/XMR, setup prices at 1 BTC/XMR: 1% of the XMR
        let outcome = setup_with_rates(btc(1.0), btc(0.01)).await?;
        assert!(outcome.bob_rejected, "{:?}", outcome.bob.as_ref().err());
        let message = outcome.bob.err().unwrap();
        assert!(message.contains("seller's offer refused"), "{message}");
        assert!(!outcome.lock_inputs_checked, "nothing may be reserved or signed");
        assert!(!outcome.alice_finished, "the maker must not end up with a swap");
        Ok(())
    }

    #[tokio::test]
    async fn a_maker_offering_dust_is_refused() -> anyhow::Result<()> {
        // 20 million BTC per XMR: a few hundred piconero for 0.005 BTC
        let outcome = setup_with_rates(btc(20_000_000.0), btc(0.01)).await?;
        assert!(outcome.bob_rejected, "{:?}", outcome.bob.as_ref().err());
        assert!(!outcome.lock_inputs_checked);
        Ok(())
    }

    #[tokio::test]
    async fn a_zero_quoted_price_is_refused() -> anyhow::Result<()> {
        let outcome = setup_with_rates(btc(0.01), Amount::ZERO).await?;
        assert!(outcome.bob_rejected, "{:?}", outcome.bob.as_ref().err());
        Ok(())
    }
}
