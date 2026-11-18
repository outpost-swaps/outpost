use crate::database::SqliteDatabase;
use crate::protocol_v5::{core, db, env, machine, ownership_proof};
use anyhow::Result;
use bitcoin::Amount;
use bitcoin_wallet::TestWalletBuilder;
use env::env::GetConfig;
use rand::rngs::OsRng;
use std::fs::File;
use uuid::Uuid;

#[tokio::test]
async fn bob_setup_completes_with_outpost_wallet_and_persists_state() -> Result<()> {
    let alice_wallet = TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
        .build()
        .await;
    let bob_wallet = TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
        .build()
        .await;

    let config = env::env::Regtest::get_config();
    let btc_amount = Amount::from_sat(500_000);
    let xmr_amount = core::monero::primitives::Amount::from_pico(10_000);
    let tx_lock_fee = Amount::from_sat(1_000);
    let fee_rate = core::bitcoin::SwapFeeRate(5_000);
    let fee_limits = machine::common::swap_fee_limits(fee_rate, btc_amount)?;
    let swap_id = Uuid::new_v4();
    let mut rng = OsRng;

    let redeem_address = alice_wallet.new_address().await?;
    let punish_address = alice_wallet.new_address().await?;
    let alice_state0 = machine::alice::State0::new(
        btc_amount,
        xmr_amount,
        Amount::ZERO,
        config,
        redeem_address,
        punish_address,
        fee_rate,
        core::bitcoin::RedeemFeeMultiplier::try_from(250)?,
        fee_limits,
        false,
        &mut rng,
    );

    let bob_state0 = machine::bob::State0::new(
        swap_id,
        &mut rng,
        btc_amount,
        xmr_amount,
        core::bitcoin::CancelTimelock::new(config.bitcoin_cancel_timelock),
        core::bitcoin::PunishTimelock::new(config.bitcoin_punish_timelock),
        core::bitcoin::RemainingRefundTimelock::new(config.bitcoin_remaining_refund_timelock),
        bob_wallet.new_address().await?,
        config.monero_finality_confirmations,
        fee_limits,
        tx_lock_fee,
    );

    let message0 = bob_state0.next_message()?;
    let (_, alice_state1) = alice_state0.receive(message0)?;
    let message1 = alice_state1.next_message()?;

    let bob_state1 = bob_state0.receive(&bob_wallet, message1).await?;
    let message2 = bob_state1.next_message();
    let verified = ownership_proof::VerifiedPsbt::verify(
        message2.tx_lock_psbt.clone(),
        &message2.tx_lock_proofs,
        alice_state1.ownership_challenge(&message2.tx_lock_psbt),
    )?;
    let alice_state2 = alice_state1.receive(verified)?;
    let message3 = alice_state2.next_message()?;

    let bob_state2 = bob_state1.receive(message3)?;
    let message4 = bob_state2.next_message()?;
    let alice_state3 = alice_state2.receive(message4)?;

    let (bob_state3, tx_lock) = bob_state2.clone().lock_btc().await?;
    assert_eq!(bob_state3.tx_lock_id(), tx_lock.txid());
    let signed_lock = bob_state3
        .tx_lock
        .as_psbt()
        .clone()
        .extract_tx_unchecked_fee_rate();
    assert_eq!(signed_lock.compute_txid(), bob_state3.tx_lock_id());
    assert_eq!(bob_state3.tx_lock_id(), alice_state3.tx_lock.txid());

    // An early refund is observed from State3, then monitored after reconstructing
    // the persisted State6. Both must identify the same signed transaction.
    let early_refund_txid = bob_state3.construct_tx_early_refund().txid();
    let early_refund_state6 = bob_state3.clone().cancel(core::monero::BlockHeight { height: 0 });
    assert_eq!(early_refund_state6.construct_tx_early_refund().txid(), early_refund_txid);

    let stored = db::bob::Bob::ExecutionSetupDone {
        state2: bob_state2.clone(),
    };

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("protocol-v5-harness.db");
    File::create(&path)?;
    let database = SqliteDatabase::open(&path).await?;
    database
        .store_protocol_v5_state(swap_id, "bob", &stored)
        .await?;

    let loaded: Option<db::bob::Bob> = database.load_protocol_v5_state(swap_id, "bob").await?;
    assert_eq!(loaded, Some(stored.clone()));

    let (prepared_state3, prepared_tx) = crate::protocol_v5::bob::prepare_lock(
        &database,
        swap_id,
        bob_state2.clone(),
        core::monero::BlockHeight { height: 0 },
        &bob_wallet,
    )
    .await?;
    assert_eq!(prepared_state3.tx_lock_id(), bob_state3.tx_lock_id());
    assert_eq!(prepared_tx.compute_txid(), bob_state3.tx_lock_id());
    let prepared: Option<db::bob::Bob> = database.load_protocol_v5_state(swap_id, "bob").await?;
    assert!(matches!(
        prepared,
        Some(db::bob::Bob::BtcLockReadyToPublish { .. })
    ));

    let recovered = loaded.expect("persisted Bob state");
    let db::bob::Bob::ExecutionSetupDone { state2 } = recovered else {
        panic!("unexpected recovered state");
    };
    let (recovered_state3, recovered_tx_lock) = state2.lock_btc().await?;
    assert_eq!(recovered_state3.tx_lock_id(), recovered_tx_lock.txid());
    assert_eq!(recovered_state3.tx_lock_id(), bob_state3.tx_lock_id());

    let tx_lock_id = bob_state3.tx_lock_id();
    let transfer_proof = core::monero::TransferProof::new(
        core::monero::TxHash("headless-harness-lock".to_owned()),
        core::monero::PrivateKey::from_scalar(core::monero::Scalar::ONE),
    );
    let bob_state4 = crate::protocol_v5::bob::accept_xmr_lock(
        &database,
        swap_id,
        bob_state3.clone(),
        transfer_proof,
        core::monero::BlockHeight { height: 0 },
        None,
    )
    .await?;

    let redeem_tx = alice_state3.signed_redeem_transaction(bob_state4.tx_redeem_encsig())?;
    assert_eq!(redeem_tx.input[0].previous_output.txid, tx_lock_id);

    let loaded4: Option<db::bob::Bob> = database.load_protocol_v5_state(swap_id, "bob").await?;
    assert!(matches!(loaded4, Some(db::bob::Bob::XmrLocked { .. })));

    let bob_state6 =
        crate::protocol_v5::bob::cancel_after_lock(&database, swap_id, bob_state4.clone()).await?;
    assert!(alice_state3.signed_cancel_transaction().is_ok());
    assert!(alice_state3.signed_punish_transaction().is_ok());

    let loaded6: Option<db::bob::Bob> = database.load_protocol_v5_state(swap_id, "bob").await?;
    assert!(matches!(loaded6, Some(db::bob::Bob::BtcCancelled(_))));
    assert_eq!(bob_state6.tx_lock.txid(), tx_lock_id);

    crate::protocol_v5::bob::persist_punished(&database, swap_id, bob_state6.clone()).await?;
    let punished: Option<db::bob::Bob> = database.load_protocol_v5_state(swap_id, "bob").await?;
    assert!(matches!(
        punished,
        Some(db::bob::Bob::BtcPunished { tx_lock_id: id, .. }) if id == tx_lock_id
    ));

    Ok(())
}

#[tokio::test]
async fn bob_event_loop_persists_and_commits_setup() -> Result<()> {
    use crate::protocol_v5::event_loop::{BobEventLoop, TakerBehaviour};
    use crate::protocol_v5::feed::FixedRate;
    use crate::protocol_v5::p2p;
    use bitcoin_wallet::{BitcoinWallet, VerifyPsbtPrevouts};
    use futures::StreamExt;
    use libp2p::swarm::SwarmEvent;
    use p2p::protocols::swap_setup::alice::{
        self, LockInputCheck, LockInputCheckError, LockInputReservation, WalletSnapshot,
    };
    use p2p::protocols::swap_setup::bob::{self, NewSwap};
    use p2p::protocols::swap_setup::verification::LockInputVerifier;
    use p2p::test::{new_swarm, SwarmExt};

    struct NoopPrevouts;

    #[async_trait::async_trait]
    impl VerifyPsbtPrevouts for NoopPrevouts {
        async fn verify_psbt_prevouts(
            &self,
            _: &bitcoin::psbt::Psbt,
            _: &tokio::sync::watch::Receiver<bool>,
        ) -> std::result::Result<(), bitcoin_wallet::PsbtPrevoutVerificationError> {
            Ok(())
        }
    }

    let config = env::env::Regtest::get_config();
    let btc = Amount::from_sat(500_000);
    let fee_limits = machine::common::swap_fee_limits(core::bitcoin::SwapFeeRate(5_000), btc)?;
    let alice_wallet = TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
        .build()
        .await;
    let bob_wallet: std::sync::Arc<dyn BitcoinWallet> = std::sync::Arc::new(
        TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
            .build()
            .await,
    );
    let snapshot = WalletSnapshot::new(
        core::monero::Amount::from_pico(1_000_000_000_000),
        alice_wallet.new_address().await?,
        alice_wallet.new_address().await?,
        bitcoin::FeeRate::from_sat_per_vb(1).unwrap(),
        core::bitcoin::SwapFeeRate(5_000),
        core::bitcoin::RedeemFeeMultiplier::try_from(250)?,
        fee_limits,
    );

    let verifier = LockInputVerifier::spawn(std::sync::Arc::new(NoopPrevouts));
    let mut alice_swarm = new_swarm(|_| {
        alice::Behaviour::new(
            Amount::from_sat(1),
            Amount::ONE_BTC,
            config,
            FixedRate::default(),
            false,
            verifier,
        )
    });
    let mut bob_swarm = new_swarm(|_| TakerBehaviour {
        quote: p2p::protocols::quote::bob(),
        swap_setup: bob::Behaviour::new(config, bob_wallet.clone()),
        transfer_proof: p2p::protocols::transfer_proof::bob(),
        encrypted_signature: p2p::protocols::encrypted_signature::bob(),
    });

    let alice_peer_id = *alice_swarm.local_peer_id();
    let bob_peer_id = *bob_swarm.local_peer_id();
    alice_swarm.listen_on_random_memory_address().await;
    bob_swarm.block_on_connection(&mut alice_swarm).await;

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("protocol-v5-event-loop.db");
    File::create(&path)?;
    let database = std::sync::Arc::new(SqliteDatabase::open(&path).await?);

    let (event_loop, handle) = BobEventLoop::new(bob_swarm, alice_peer_id, database.clone(), "/dns4/localhost/tcp/1".parse()?);
    let event_loop_task = tokio::spawn(event_loop.run());

    let (alice_txid_tx, alice_txid_rx) = tokio::sync::oneshot::channel();
    let mut alice_txid_tx = Some(alice_txid_tx);
    let alice_task = tokio::spawn(async move {
        loop {
            let event = alice_swarm.select_next_some().await;
            let SwarmEvent::Behaviour(alice::OutEvent::Initiated {
                mut send_wallet_snapshot,
                mut check_lock_inputs,
                mut register_swap,
                ..
            }) = event
            else {
                continue;
            };

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
                let (release_sender, _release_receiver) = tokio::sync::mpsc::unbounded_channel();
                responder.respond(Ok(LockInputReservation::new(outpoints, release_sender)))?;

                let (register, responder) = register_swap.recv().await?;
                if let Some(alice_txid_tx) = alice_txid_tx {
                    let _ = alice_txid_tx.send(register.state3.tx_lock.txid());
                }
                responder.respond(Ok(()))?;
                anyhow::Ok(())
            });
        }
    });

    let swap_id = Uuid::new_v4();
    let state2 = handle
        .start_swap(NewSwap {
            swap_id,
            btc,
            tx_lock_fee: Amount::from_sat(1_000),
            fee_limits,
            bitcoin_refund_address: bob_wallet.new_address().await?,
            quoted_price: Amount::from_btc(0.01)?,
        })
        .await?;

    handle.commit().await?;
    let alice_txid = alice_txid_rx.await?;
    let bob_txid = state2.clone().lock_btc().await?.0.tx_lock_id();
    assert_eq!(alice_txid, bob_txid);

    let stored: Option<db::bob::Bob> = database.load_protocol_v5_state(swap_id, "bob").await?;
    let db::bob::Bob::ExecutionSetupDone { state2 } = stored.expect("persisted setup state") else {
        panic!("unexpected persisted state");
    };
    let recovered_txid = state2.clone().lock_btc().await?.0.tx_lock_id();
    assert_eq!(recovered_txid, bob_txid);

    event_loop_task.abort();
    alice_task.abort();
    Ok(())
}

#[tokio::test]
async fn bob_event_loop_requests_quote() -> Result<()> {
    use crate::protocol_v5::event_loop::{BobEventLoop, TakerBehaviour};
    use crate::protocol_v5::p2p;
    use futures::StreamExt;
    use libp2p::request_response::{Event, Message};
    use libp2p::swarm::SwarmEvent;
    use p2p::protocols::quote::{BidQuote, RefundPolicyWire};
    use p2p::protocols::swap_setup::bob;
    use p2p::test::{new_swarm, SwarmExt};

    let config = env::env::Regtest::get_config();
    let mut alice_swarm = new_swarm(|_| p2p::protocols::quote::alice(None));
    let bob_wallet: std::sync::Arc<dyn bitcoin_wallet::BitcoinWallet> =
        std::sync::Arc::new(TestWalletBuilder::new(100_000).build().await);
    let mut bob_swarm = new_swarm(|_| TakerBehaviour {
        quote: p2p::protocols::quote::bob(),
        swap_setup: bob::Behaviour::new(config, bob_wallet.clone()),
        transfer_proof: p2p::protocols::transfer_proof::bob(),
        encrypted_signature: p2p::protocols::encrypted_signature::bob(),
    });

    let alice_peer_id = *alice_swarm.local_peer_id();
    alice_swarm.listen_on_random_memory_address().await;
    bob_swarm.block_on_connection(&mut alice_swarm).await;

    let quote = BidQuote {
        price: Amount::from_btc(0.01)?,
        min_quantity: Amount::from_sat(1_000),
        max_quantity: Amount::from_sat(1_000_000),
        refund_policy: RefundPolicyWire::FullRefund,
        reserve_proof: None,
    };

    let expected_quote = quote.clone();
    let alice_task = tokio::spawn(async move {
        loop {
            let SwarmEvent::Behaviour(Event::Message {
                message:
                    Message::Request {
                        request: (),
                        channel,
                        ..
                    },
                ..
            }) = alice_swarm.select_next_some().await
            else {
                continue;
            };
            let _ = alice_swarm
                .behaviour_mut()
                .send_response(channel, quote.clone());
        }
    });

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("protocol-v5-quote.db");
    File::create(&path)?;
    let database = std::sync::Arc::new(SqliteDatabase::open(&path).await?);
    let (event_loop, handle) = BobEventLoop::new(bob_swarm, alice_peer_id, database, "/dns4/localhost/tcp/1".parse()?);
    let event_loop_task = tokio::spawn(event_loop.run());

    let received = handle.request_quote().await?;
    assert_eq!(received, expected_quote);

    event_loop_task.abort();
    alice_task.abort();
    Ok(())
}

#[tokio::test]
async fn bob_and_maker_event_loops_complete_setup() -> Result<()> {
    use crate::protocol_v5::event_loop::{BobEventLoop, MakerBehaviour, MakerEventLoop, TakerBehaviour};
    use crate::protocol_v5::feed::FixedRate;
    use crate::protocol_v5::p2p;
    use bitcoin_wallet::{BitcoinWallet, VerifyPsbtPrevouts};
    use p2p::protocols::quote::{BidQuote, RefundPolicyWire};
    use p2p::protocols::swap_setup::alice::{self, WalletSnapshot};
    use p2p::protocols::swap_setup::bob::{self, NewSwap};
    use p2p::protocols::swap_setup::verification::LockInputVerifier;
    use p2p::test::{new_swarm, SwarmExt};

    struct NoopPrevouts;

    #[async_trait::async_trait]
    impl VerifyPsbtPrevouts for NoopPrevouts {
        async fn verify_psbt_prevouts(
            &self,
            _: &bitcoin::psbt::Psbt,
            _: &tokio::sync::watch::Receiver<bool>,
        ) -> std::result::Result<(), bitcoin_wallet::PsbtPrevoutVerificationError> {
            Ok(())
        }
    }

    let config = env::env::Regtest::get_config();
    let btc = Amount::from_sat(500_000);
    let fee_limits = machine::common::swap_fee_limits(core::bitcoin::SwapFeeRate(5_000), btc)?;
    let alice_wallet = TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
        .build()
        .await;
    let bob_wallet: std::sync::Arc<dyn BitcoinWallet> = std::sync::Arc::new(
        TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
            .build()
            .await,
    );
    let snapshot = WalletSnapshot::new(
        core::monero::Amount::from_pico(1_000_000_000_000),
        alice_wallet.new_address().await?,
        alice_wallet.new_address().await?,
        bitcoin::FeeRate::from_sat_per_vb(1).unwrap(),
        core::bitcoin::SwapFeeRate(5_000),
        core::bitcoin::RedeemFeeMultiplier::try_from(250)?,
        fee_limits,
    );
    let quote = BidQuote {
        price: Amount::from_btc(0.01)?,
        min_quantity: Amount::from_sat(1_000),
        max_quantity: Amount::ONE_BTC,
        refund_policy: RefundPolicyWire::FullRefund,
        reserve_proof: None,
    };

    let verifier = LockInputVerifier::spawn(std::sync::Arc::new(NoopPrevouts));
    let mut alice_swarm = new_swarm(|identity| {
        MakerBehaviour::new(
            Amount::from_sat(1),
            Amount::ONE_BTC,
            config,
            FixedRate::default(),
            false,
            verifier,
            identity,
            Vec::new(),
            p2p::protocols::rendezvous::XmrBtcNamespace::Testnet,
            libp2p::connection_limits::ConnectionLimits::default(),
        )
    });
    let mut bob_swarm = new_swarm(|_| TakerBehaviour {
        quote: p2p::protocols::quote::bob(),
        swap_setup: bob::Behaviour::new(config, bob_wallet.clone()),
        transfer_proof: p2p::protocols::transfer_proof::bob(),
        encrypted_signature: p2p::protocols::encrypted_signature::bob(),
    });

    let alice_peer_id = *alice_swarm.local_peer_id();
    let bob_peer_id = *bob_swarm.local_peer_id();
    alice_swarm.listen_on_random_memory_address().await;
    bob_swarm.block_on_connection(&mut alice_swarm).await;

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("protocol-v5-maker.db");
    File::create(&path)?;
    let database = std::sync::Arc::new(SqliteDatabase::open(&path).await?);

    struct FixedQuote(BidQuote);

    #[async_trait::async_trait]
    impl crate::protocol_v5::event_loop::MakerQuoteProvider for FixedQuote {
        async fn quote(&self) -> Result<BidQuote> {
            Ok(self.0.clone())
        }
    }

    struct FixedSnapshot(WalletSnapshot);

    #[async_trait::async_trait]
    impl crate::protocol_v5::event_loop::MakerWalletSnapshotProvider for FixedSnapshot {
        async fn snapshot(
            &self,
            _btc_amount: core::bitcoin::Amount,
        ) -> Result<crate::protocol_v5::event_loop::MakerSnapshot> {
            Ok(crate::protocol_v5::event_loop::MakerSnapshot {
                snapshot: self.0.clone(),
                anti_spam_deposit: core::bitcoin::Amount::from_sat(0),
                hermes_enabled: false,
            })
        }

        async fn reserve_for_swap(
            &self,
            _swap_id: Uuid,
            _btc_amount: core::bitcoin::Amount,
            _xmr_amount: core::monero::Amount,
        ) -> Result<bool> {
            Ok(true)
        }

        async fn release_liquidity(&self, _swap_id: Uuid) {}
    }

    let (maker_loop, mut maker_handle) = MakerEventLoop::new(
        alice_swarm,
        database.clone(),
        std::sync::Arc::new(FixedQuote(quote.clone())),
        std::sync::Arc::new(FixedSnapshot(snapshot.clone())),
    );
    let maker_task = tokio::spawn(maker_loop.run());
    let (bob_loop, mut bob_handle) = BobEventLoop::new(bob_swarm, alice_peer_id, database.clone(), "/dns4/localhost/tcp/1".parse()?);
    let bob_task = tokio::spawn(bob_loop.run());

    assert_eq!(bob_handle.request_quote().await?, quote);

    let swap_id = Uuid::new_v4();
    let state2 = bob_handle
        .start_swap(NewSwap {
            swap_id,
            btc,
            tx_lock_fee: Amount::from_sat(1_000),
            fee_limits,
            bitcoin_refund_address: bob_wallet.new_address().await?,
            quoted_price: Amount::from_btc(0.01)?,
        })
        .await?;
    bob_handle.commit().await?;

    let mut registered = maker_handle.next_swap().await?;
    assert_eq!(registered.swap_id, swap_id);
    assert_eq!(registered.handle().peer_id, bob_peer_id);
    let bob_txid = state2.clone().lock_btc().await?.0.tx_lock_id();
    assert_eq!(registered.state3.tx_lock.txid(), bob_txid);

    let stored: Option<db::alice::Alice> =
        database.load_protocol_v5_state(swap_id, "alice").await?;
    let db::alice::Alice::Started { state3 } = stored.expect("persisted Alice setup state") else {
        panic!("unexpected persisted Alice state");
    };
    assert_eq!(state3.tx_lock.txid(), bob_txid);

    let proof = core::monero::TransferProof::new(
        core::monero::TxHash("maker-handle-proof".to_owned()),
        core::monero::PrivateKey::from_scalar(core::monero::Scalar::ONE),
    );
    let mut send_proof = Box::pin(registered.handle().send_transfer_proof(proof.clone()));
    let received = tokio::select! {
        received = bob_handle.recv_transfer_proof() => received?,
        sent = &mut send_proof => {
            sent?;
            panic!("maker sent the transfer proof before Bob received it");
        }
    };
    assert_eq!(received.swap_id, swap_id);
    assert_eq!(received.tx_lock_proof.tx_hash().0, proof.tx_hash().0);
    bob_handle.ack_transfer_proof().await?;
    send_proof.await?;

    maker_task.abort();
    bob_task.abort();

    let mut resumed_swarm = new_swarm(|identity| {
        MakerBehaviour::new(
            Amount::from_sat(1),
            Amount::ONE_BTC,
            config,
            FixedRate::default(),
            false,
            LockInputVerifier::spawn(std::sync::Arc::new(NoopPrevouts)),
            identity,
            Vec::new(),
            p2p::protocols::rendezvous::XmrBtcNamespace::Testnet,
            libp2p::connection_limits::ConnectionLimits::default(),
        )
    });
    resumed_swarm.listen_on_random_memory_address().await;
    let (resumed_loop, mut resumed_handle) = MakerEventLoop::new(
        resumed_swarm,
        database.clone(),
        std::sync::Arc::new(FixedQuote(quote)),
        std::sync::Arc::new(FixedSnapshot(snapshot)),
    );
    let resumed_task = tokio::spawn(resumed_loop.run());
    let mut resumed = resumed_handle
        .resume_swap(crate::protocol_v5::event_loop::ResumeAliceSwap {
            swap_id,
            peer_id: bob_peer_id,
            state3,
        })
        .await?;
    assert_eq!(resumed.swap_id, swap_id);
    assert_eq!(resumed.state3.tx_lock.txid(), bob_txid);
    resumed_task.abort();
    bob_task.abort();
    Ok(())
}

#[tokio::test]
async fn maker_buffers_transfer_proof_until_the_taker_reconnects() -> Result<()> {
    use crate::protocol_v5::event_loop::{BobEventLoop, MakerBehaviour, MakerEventLoop, TakerBehaviour};
    use crate::protocol_v5::feed::FixedRate;
    use crate::protocol_v5::p2p;
    use bitcoin_wallet::{BitcoinWallet, VerifyPsbtPrevouts};
    use p2p::protocols::quote::{BidQuote, RefundPolicyWire};
    use p2p::protocols::swap_setup::alice::WalletSnapshot;
    use p2p::protocols::swap_setup::bob::{self, NewSwap};
    use p2p::protocols::swap_setup::verification::LockInputVerifier;
    use p2p::test::{new_swarm, new_swarm_with_identity, SwarmExt};

    struct NoopPrevouts;

    #[async_trait::async_trait]
    impl VerifyPsbtPrevouts for NoopPrevouts {
        async fn verify_psbt_prevouts(
            &self,
            _: &bitcoin::psbt::Psbt,
            _: &tokio::sync::watch::Receiver<bool>,
        ) -> std::result::Result<(), bitcoin_wallet::PsbtPrevoutVerificationError> {
            Ok(())
        }
    }

    let config = env::env::Regtest::get_config();
    let btc = Amount::from_sat(500_000);
    let fee_limits = machine::common::swap_fee_limits(core::bitcoin::SwapFeeRate(5_000), btc)?;
    let alice_wallet = TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
        .build()
        .await;
    let bob_wallet: std::sync::Arc<dyn BitcoinWallet> = std::sync::Arc::new(
        TestWalletBuilder::new(Amount::ONE_BTC.to_sat())
            .build()
            .await,
    );
    let snapshot = WalletSnapshot::new(
        core::monero::Amount::from_pico(1_000_000_000_000),
        alice_wallet.new_address().await?,
        alice_wallet.new_address().await?,
        bitcoin::FeeRate::from_sat_per_vb(1).unwrap(),
        core::bitcoin::SwapFeeRate(5_000),
        core::bitcoin::RedeemFeeMultiplier::try_from(250)?,
        fee_limits,
    );
    let quote = BidQuote {
        price: Amount::from_btc(0.01)?,
        min_quantity: Amount::from_sat(1_000),
        max_quantity: Amount::ONE_BTC,
        refund_policy: RefundPolicyWire::FullRefund,
        reserve_proof: None,
    };

    let verifier = LockInputVerifier::spawn(std::sync::Arc::new(NoopPrevouts));
    let mut alice_swarm = new_swarm(|identity| {
        MakerBehaviour::new(
            Amount::from_sat(1),
            Amount::ONE_BTC,
            config,
            FixedRate::default(),
            false,
            verifier,
            identity,
            Vec::new(),
            p2p::protocols::rendezvous::XmrBtcNamespace::Testnet,
            libp2p::connection_limits::ConnectionLimits::default(),
        )
    });

    // Keep the taker identity so it can reconnect as the same peer.
    let bob_identity = libp2p::identity::Keypair::generate_ed25519();
    let mut bob_swarm = new_swarm_with_identity(bob_identity.clone(), |_| TakerBehaviour {
        quote: p2p::protocols::quote::bob(),
        swap_setup: bob::Behaviour::new(config, bob_wallet.clone()),
        transfer_proof: p2p::protocols::transfer_proof::bob(),
        encrypted_signature: p2p::protocols::encrypted_signature::bob(),
    });

    let alice_peer_id = *alice_swarm.local_peer_id();
    let bob_peer_id = *bob_swarm.local_peer_id();
    alice_swarm.listen_on_random_memory_address().await;
    bob_swarm.block_on_connection(&mut alice_swarm).await;

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("protocol-v5-proof-buffer.db");
    File::create(&path)?;
    let database = std::sync::Arc::new(SqliteDatabase::open(&path).await?);

    struct FixedQuote(BidQuote);

    #[async_trait::async_trait]
    impl crate::protocol_v5::event_loop::MakerQuoteProvider for FixedQuote {
        async fn quote(&self) -> Result<BidQuote> {
            Ok(self.0.clone())
        }
    }

    struct FixedSnapshot(WalletSnapshot);

    #[async_trait::async_trait]
    impl crate::protocol_v5::event_loop::MakerWalletSnapshotProvider for FixedSnapshot {
        async fn snapshot(
            &self,
            _btc_amount: core::bitcoin::Amount,
        ) -> Result<crate::protocol_v5::event_loop::MakerSnapshot> {
            Ok(crate::protocol_v5::event_loop::MakerSnapshot {
                snapshot: self.0.clone(),
                anti_spam_deposit: core::bitcoin::Amount::from_sat(0),
                hermes_enabled: false,
            })
        }

        async fn reserve_for_swap(
            &self,
            _swap_id: Uuid,
            _btc_amount: core::bitcoin::Amount,
            _xmr_amount: core::monero::Amount,
        ) -> Result<bool> {
            Ok(true)
        }

        async fn release_liquidity(&self, _swap_id: Uuid) {}
    }

    let (maker_loop, mut maker_handle) = MakerEventLoop::new(
        alice_swarm,
        database.clone(),
        std::sync::Arc::new(FixedQuote(quote.clone())),
        std::sync::Arc::new(FixedSnapshot(snapshot.clone())),
    );
    let maker_task = tokio::spawn(maker_loop.run());
    let (bob_loop, mut bob_handle) =
        BobEventLoop::new(bob_swarm, alice_peer_id, database.clone(), "/dns4/localhost/tcp/1".parse()?);
    let bob_task = tokio::spawn(bob_loop.run());

    assert_eq!(bob_handle.request_quote().await?, quote);
    let swap_id = Uuid::new_v4();
    let state2 = bob_handle
        .start_swap(NewSwap {
            swap_id,
            btc,
            tx_lock_fee: Amount::from_sat(1_000),
            fee_limits,
            bitcoin_refund_address: bob_wallet.new_address().await?,
            quoted_price: Amount::from_btc(0.01)?,
        })
        .await?;
    bob_handle.commit().await?;
    let mut registered = maker_handle.next_swap().await?;
    let state3 = registered.state3.clone();
    maker_task.abort();
    bob_task.abort();

    // Resume the maker with no taker connected: the proof must be buffered,
    // not failed with "Failed to dial the requested peer".
    let mut resumed_swarm = new_swarm(|identity| {
        MakerBehaviour::new(
            Amount::from_sat(1),
            Amount::ONE_BTC,
            config,
            FixedRate::default(),
            false,
            LockInputVerifier::spawn(std::sync::Arc::new(NoopPrevouts)),
            identity,
            Vec::new(),
            p2p::protocols::rendezvous::XmrBtcNamespace::Testnet,
            libp2p::connection_limits::ConnectionLimits::default(),
        )
    });
    let resumed_addr = resumed_swarm.listen_on_random_memory_address().await;
    let (resumed_loop, mut resumed_handle) = MakerEventLoop::new(
        resumed_swarm,
        database.clone(),
        std::sync::Arc::new(FixedQuote(quote)),
        std::sync::Arc::new(FixedSnapshot(snapshot)),
    );
    let resumed_task = tokio::spawn(resumed_loop.run());
    let mut resumed = resumed_handle
        .resume_swap(crate::protocol_v5::event_loop::ResumeAliceSwap {
            swap_id,
            peer_id: bob_peer_id,
            state3,
        })
        .await?;

    let proof = core::monero::TransferProof::new(
        core::monero::TxHash("buffered-proof".to_owned()),
        core::monero::PrivateKey::from_scalar(core::monero::Scalar::ONE),
    );
    let mut send_proof = Box::pin(resumed.handle().send_transfer_proof(proof.clone()));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(500), &mut send_proof)
            .await
            .is_err(),
        "the maker must buffer the proof while the taker is disconnected"
    );

    // Reconnect as the same taker peer; the buffered proof must be flushed.
    let bob_swarm = new_swarm_with_identity(bob_identity, |_| TakerBehaviour {
        quote: p2p::protocols::quote::bob(),
        swap_setup: bob::Behaviour::new(config, bob_wallet.clone()),
        transfer_proof: p2p::protocols::transfer_proof::bob(),
        encrypted_signature: p2p::protocols::encrypted_signature::bob(),
    });
    let (bob_loop, mut bob_handle) =
        BobEventLoop::new(bob_swarm, alice_peer_id, database.clone(), resumed_addr);
    let bob_task = tokio::spawn(bob_loop.run());

    let received = tokio::time::timeout(std::time::Duration::from_secs(15), bob_handle.recv_transfer_proof())
        .await
        .expect("buffered proof must arrive after the taker reconnects")?;
    assert_eq!(received.swap_id, swap_id);
    assert_eq!(received.tx_lock_proof.tx_hash().0, proof.tx_hash().0);
    bob_handle.ack_transfer_proof().await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), send_proof)
        .await
        .expect("maker send must complete after the ack")?;

    resumed_task.abort();
    bob_task.abort();
    Ok(())
}

#[tokio::test]
async fn bob_event_loop_redials_seller_until_transfer_proof_arrives() -> Result<()> {
    use crate::protocol_v5::event_loop::{BobEventLoop, MakerBehaviour, TakerBehaviour};
    use crate::protocol_v5::feed::FixedRate;
    use crate::protocol_v5::p2p;
    use bitcoin_wallet::{BitcoinWallet, VerifyPsbtPrevouts};
    use futures::StreamExt;
    use libp2p::swarm::SwarmEvent;
    use p2p::protocols::swap_setup::verification::LockInputVerifier;
    use p2p::protocols::swap_setup::bob;
    use p2p::test::{new_swarm, SwarmExt};

    struct NoopPrevouts;

    #[async_trait::async_trait]
    impl VerifyPsbtPrevouts for NoopPrevouts {
        async fn verify_psbt_prevouts(
            &self,
            _: &bitcoin::psbt::Psbt,
            _: &tokio::sync::watch::Receiver<bool>,
        ) -> std::result::Result<(), bitcoin_wallet::PsbtPrevoutVerificationError> {
            Ok(())
        }
    }

    // Regression for swap c365206b: the maker pushes the transfer proof
    // Outbound-only. When the setup connection drops during a long
    // confirmation wait, the proof used to stay buffered forever and Bob
    // timed out. The taker event loop must keep re-dialing the seller's
    // address until the proof is delivered.
    let config = env::env::Regtest::get_config();
    let bob_wallet: std::sync::Arc<dyn BitcoinWallet> =
        std::sync::Arc::new(TestWalletBuilder::new(100_000).build().await);

    let mut alice_swarm = new_swarm(|identity| {
        MakerBehaviour::new(
            Amount::from_sat(1),
            Amount::ONE_BTC,
            config.clone(),
            FixedRate::default(),
            false,
            LockInputVerifier::spawn(std::sync::Arc::new(NoopPrevouts)),
            identity,
            Vec::new(),
            p2p::protocols::rendezvous::XmrBtcNamespace::Testnet,
            libp2p::connection_limits::ConnectionLimits::default(),
        )
    });
    let alice_addr = alice_swarm.listen_on_random_memory_address().await;
    let alice_peer_id = *alice_swarm.local_peer_id();

    let mut bob_swarm = new_swarm(|_| TakerBehaviour {
        quote: p2p::protocols::quote::bob(),
        swap_setup: bob::Behaviour::new(config, bob_wallet.clone()),
        transfer_proof: p2p::protocols::transfer_proof::bob(),
        encrypted_signature: p2p::protocols::encrypted_signature::bob(),
    });
    // Intentionally NOT connected: the first connection was "lost".

    let dir = tempfile::tempdir()?;
    let path = dir.path().join("protocol-v5-redial.db");
    File::create(&path)?;
    let database = std::sync::Arc::new(SqliteDatabase::open(&path).await?);
    let (event_loop, _handle) =
        BobEventLoop::new(bob_swarm, alice_peer_id, database, alice_addr.clone());
    let _bob_task = tokio::spawn(event_loop.run());

    // The redial tick fires every 5s; the test passes once Alice sees an
    // inbound connection from Bob after the initial "loss".
    let saw_redial = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let event = alice_swarm.select_next_some().await;
            if let SwarmEvent::IncomingConnection { .. } = event {
                break;
            }
        }
    })
    .await;
    assert!(
        saw_redial.is_ok(),
        "Bob event loop must re-dial the seller while waiting for the transfer proof"
    );

    Ok(())
}

/// Audit H-3 follow-up: the cancel/refund loop watches the seller's early refund,
/// as upstream does. A real `State6` from an in-memory setup, a real SQLite store,
/// and a wallet whose chain answers are scripted.
mod cancel_path_early_refund {
    use super::*;
    use bitcoin_wallet::{ScriptStatus, Subscription, Watchable};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Answers chain queries from a script and records every broadcast attempt.
    pub(super) struct ScriptedChain {
        statuses: Mutex<HashMap<::bitcoin::Txid, ScriptStatus>>,
        pub(super) broadcasts: Arc<Mutex<Vec<String>>>,
        accept_broadcasts: bool,
    }

    impl ScriptedChain {
        pub(super) fn new(scripted: &[(::bitcoin::Txid, ScriptStatus)]) -> Self {
            Self {
                statuses: Mutex::new(scripted.iter().cloned().collect()),
                broadcasts: Arc::new(Mutex::new(Vec::new())),
                accept_broadcasts: false,
            }
        }

        /// Broadcasts succeed; the transaction then shows as in the mempool.
        pub(super) fn accepting(mut self) -> Self {
            self.accept_broadcasts = true;
            self
        }
    }

    #[async_trait::async_trait]
    impl bitcoin_wallet::BitcoinWallet for ScriptedChain {
        async fn balance(&self) -> Result<Amount> { unimplemented!() }
        async fn balance_info(&self) -> Result<bdk_wallet::Balance> { unimplemented!() }
        async fn new_address(&self) -> Result<::bitcoin::Address> { unimplemented!() }
        async fn send_to_address(&self, _: ::bitcoin::Address, _: Amount, _: Amount, _: Option<::bitcoin::Address>, _: Option<usize>) -> Result<::bitcoin::Psbt> { unimplemented!() }
        async fn send_to_address_dynamic_fee(&self, _: ::bitcoin::Address, _: Amount, _: Option<::bitcoin::Address>, _: Option<usize>) -> Result<::bitcoin::psbt::Psbt> { unimplemented!() }
        async fn sweep_balance_to_address_dynamic_fee(&self, _: ::bitcoin::Address) -> Result<::bitcoin::psbt::Psbt> { unimplemented!() }
        async fn sign_and_finalize(&self, _: ::bitcoin::psbt::Psbt) -> Result<::bitcoin::Transaction> { unimplemented!() }
        fn prove_utxo_ownership(&self, _: &::bitcoin::psbt::Psbt, _: ownership_proof::Challenge) -> Result<Vec<ownership_proof::OwnershipProof>> { unimplemented!() }
        async fn ensure_broadcasted(&self, transaction: ::bitcoin::Transaction, kind: &str) -> Result<(::bitcoin::Txid, Subscription)> {
            self.broadcasts.lock().unwrap().push(kind.to_string());
            if !self.accept_broadcasts {
                anyhow::bail!("scripted chain: broadcast of {kind} not accepted");
            }
            let txid = transaction.compute_txid();
            self.statuses.lock().unwrap().insert(txid, ScriptStatus::InMempool);
            let (_sender, receiver) = tokio::sync::watch::channel(ScriptStatus::InMempool);
            Ok((txid, Subscription::new(receiver, 1, txid)))
        }
        async fn sync(&self) -> Result<()> { unimplemented!() }
        async fn health_check(&self) -> Result<()> { unimplemented!() }
        async fn subscribe_to(&self, _: Box<dyn Watchable>) -> Subscription { unimplemented!() }
        async fn status_of_script(&self, tx: &dyn Watchable) -> Result<ScriptStatus> {
            Ok(*self.statuses.lock().unwrap().get(&tx.id()).unwrap_or(&ScriptStatus::Unseen))
        }
        async fn get_raw_transaction(&self, _: ::bitcoin::Txid) -> Result<Option<Arc<::bitcoin::Transaction>>> { Ok(None) }
        async fn max_giveable(&self, _: usize, _: Option<usize>) -> Result<(Amount, Amount)> { unimplemented!() }
        async fn estimate_fee(&self, _: ::bitcoin::Weight, _: Option<Amount>) -> Result<Amount> { unimplemented!() }
        async fn swap_fee_rate(&self) -> Result<::bitcoin::FeeRate> { unimplemented!() }
        fn network(&self) -> ::bitcoin::Network { ::bitcoin::Network::Regtest }
        fn finality_confirmations(&self) -> u32 { 1 }
        async fn wallet_export(&self, _: &str) -> Result<bdk_wallet::export::FullyNodedExport> { unimplemented!() }
    }

    /// A buyer's State6 after a completed in-memory setup, plus a store to run against.
    async fn state6_and_store() -> Result<(machine::bob::State6, SqliteDatabase, Uuid, tempfile::TempDir)> {
        state6_with_deposit(Amount::ZERO).await
    }

    /// As [`state6_and_store`], with a seller that asked for `deposit` (a partial refund).
    pub(super) async fn state6_with_deposit(
        deposit: Amount,
    ) -> Result<(machine::bob::State6, SqliteDatabase, Uuid, tempfile::TempDir)> {
        let alice_wallet = TestWalletBuilder::new(Amount::ONE_BTC.to_sat()).build().await;
        let bob_wallet = TestWalletBuilder::new(Amount::ONE_BTC.to_sat()).build().await;
        let config = env::env::Regtest::get_config();
        let btc_amount = Amount::from_sat(500_000);
        let xmr_amount = core::monero::primitives::Amount::from_pico(10_000);
        let fee_rate = core::bitcoin::SwapFeeRate(5_000);
        let fee_limits = machine::common::swap_fee_limits(fee_rate, btc_amount)?;
        let swap_id = Uuid::new_v4();
        let mut rng = OsRng;

        let alice_state0 = machine::alice::State0::new(
            btc_amount,
            xmr_amount,
            deposit,
            config,
            alice_wallet.new_address().await?,
            alice_wallet.new_address().await?,
            fee_rate,
            core::bitcoin::RedeemFeeMultiplier::try_from(250)?,
            fee_limits,
            false,
            &mut rng,
        );
        let bob_state0 = machine::bob::State0::new(
            swap_id,
            &mut rng,
            btc_amount,
            xmr_amount,
            core::bitcoin::CancelTimelock::new(config.bitcoin_cancel_timelock),
            core::bitcoin::PunishTimelock::new(config.bitcoin_punish_timelock),
            core::bitcoin::RemainingRefundTimelock::new(config.bitcoin_remaining_refund_timelock),
            bob_wallet.new_address().await?,
            config.monero_finality_confirmations,
            fee_limits,
            Amount::from_sat(1_000),
        );
        let (_, alice_state1) = alice_state0.receive(bob_state0.next_message()?)?;
        let bob_state1 = bob_state0.receive(&bob_wallet, alice_state1.next_message()?).await?;
        let message2 = bob_state1.next_message();
        let verified = ownership_proof::VerifiedPsbt::verify(
            message2.tx_lock_psbt.clone(),
            &message2.tx_lock_proofs,
            alice_state1.ownership_challenge(&message2.tx_lock_psbt),
        )?;
        let alice_state2 = alice_state1.receive(verified)?;
        let bob_state2 = bob_state1.receive(alice_state2.next_message()?)?;
        let (bob_state3, _) = bob_state2.lock_btc().await?;
        let state6 = bob_state3.cancel(core::monero::BlockHeight { height: 0 });

        let dir = tempfile::tempdir()?;
        let path = dir.path().join("cancel-path.db");
        File::create(&path)?;
        let database = SqliteDatabase::open(&path).await?;
        Ok((state6, database, swap_id, dir))
    }

    async fn run(
        database: &SqliteDatabase,
        swap_id: Uuid,
        state6: machine::bob::State6,
        chain: &ScriptedChain,
    ) -> Result<crate::protocol_v5::bob::RefundOutcome> {
        crate::protocol_v5::bob::run_cancel_and_refund(
            database,
            swap_id,
            state6,
            chain,
            |_| {},
            |_| {},
            |_| {},
        )
        .await
    }

    /// Before: the loop published a cancel that could never be valid and retried it for a
    /// week, then the swap resumed and did it again; the swap never finished.
    #[tokio::test]
    async fn a_confirmed_early_refund_ends_the_swap_without_a_cancel() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_and_store().await?;
        let early_refund = state6.construct_tx_early_refund().txid();
        let chain = ScriptedChain::new(&[(
            early_refund,
            ScriptStatus::Confirmed(bitcoin_wallet::primitives::Confirmed::new(0)),
        )]);

        let finished = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run(&database, swap_id, state6, &chain),
        )
        .await
        .expect("a confirmed early refund must end the loop at once")?;

        assert_eq!(finished, crate::protocol_v5::bob::RefundOutcome::Refunded(early_refund));
        assert!(chain.broadcasts.lock().unwrap().is_empty(), "no cancel or refund may be broadcast");
        let stored: Option<db::bob::Bob> = database.load_protocol_v5_state(swap_id, "bob").await?;
        assert!(
            matches!(
                stored,
                Some(db::bob::Bob::Done(db::bob::BobEndState::BtcEarlyRefunded(_)))
            ),
            "{stored:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn an_early_refund_in_the_mempool_holds_back_the_cancel() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_and_store().await?;
        let early_refund = state6.construct_tx_early_refund().txid();
        let chain = ScriptedChain::new(&[(early_refund, ScriptStatus::InMempool)]);

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            run(&database, swap_id, state6, &chain),
        )
        .await;

        assert!(outcome.is_err(), "the loop must keep waiting for the early refund");
        assert!(
            chain.broadcasts.lock().unwrap().is_empty(),
            "a cancel would conflict with the early refund in the mempool"
        );
        assert_eq!(database.load_protocol_v5_state::<db::bob::Bob>(swap_id, "bob").await?, None);
        Ok(())
    }

    #[tokio::test]
    async fn without_an_early_refund_the_cancel_is_published_as_before() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_and_store().await?;
        let chain = ScriptedChain::new(&[]);

        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            run(&database, swap_id, state6, &chain),
        )
        .await;

        assert_eq!(chain.broadcasts.lock().unwrap().first().map(String::as_str), Some("cancel"));
        Ok(())
    }
}

/// Audit H-1 (the real gap): after a partial refund the deposit is claimed back, unless
/// the seller burns it. The real `reclaim_deposit` against a partial-refund State6 from
/// an in-memory setup with a 2% deposit, a real SQLite store and a scripted chain.
mod partial_refund_deposit {
    use super::cancel_path_early_refund::{state6_with_deposit, ScriptedChain};
    use super::*;
    use crate::protocol_v5::bob::{reclaim_deposit, RefundOutcome};
    use bitcoin_wallet::primitives::Confirmed;
    use bitcoin_wallet::ScriptStatus;

    const DEPOSIT: Amount = Amount::from_sat(10_000); // 2% of the 500,000-sat test swap

    fn confirmations(n: u32) -> ScriptStatus {
        ScriptStatus::Confirmed(Confirmed::new(n - 1))
    }

    async fn stored(database: &SqliteDatabase, swap_id: Uuid) -> Result<Option<db::bob::Bob>> {
        database.load_protocol_v5_state(swap_id, "bob").await
    }

    async fn run_for(
        seconds: u64,
        database: &SqliteDatabase,
        swap_id: Uuid,
        state6: &machine::bob::State6,
        chain: &ScriptedChain,
    ) -> Option<Result<RefundOutcome>> {
        tokio::time::timeout(
            std::time::Duration::from_secs(seconds),
            reclaim_deposit(database, swap_id, state6, chain, &|_| {}),
        )
        .await
        .ok()
    }

    #[tokio::test]
    async fn a_partial_refund_setup_has_the_deposit_and_its_signatures() -> Result<()> {
        let (state6, _database, _swap_id, _dir) = state6_with_deposit(DEPOSIT).await?;
        assert_eq!(state6.btc_amnesty_amount, Some(DEPOSIT));
        // a deposit seller signs no full refund, so the best refund is the partial one
        let (_, refund_type) = state6.construct_best_bitcoin_refund_tx()?;
        assert!(matches!(refund_type, machine::bob::RefundType::Partial { .. }));
        // the reclaim is fully signable by the buyer
        state6.signed_amnesty_transaction()?;
        Ok(())
    }

    #[tokio::test]
    async fn a_partial_refund_is_recorded_as_partial() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_with_deposit(DEPOSIT).await?;
        let chain = ScriptedChain::new(&[]).accepting();

        let (_, _, refund_type) =
            crate::protocol_v5::bob::publish_refund(&database, swap_id, &state6, &chain).await?;

        assert!(matches!(refund_type, machine::bob::RefundType::Partial { .. }));
        assert!(matches!(
            stored(&database, swap_id).await?,
            Some(db::bob::Bob::BtcPartialRefundPublished(_))
        ));
        Ok(())
    }

    /// Before: a confirmed partial refund was stored as `Done(BtcRefunded)` and shown
    /// as refunded, and the deposit was never claimed.
    #[tokio::test]
    async fn a_confirmed_partial_refund_goes_on_to_the_deposit() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_with_deposit(DEPOSIT).await?;
        let (_, refund_type) = state6.construct_best_bitcoin_refund_tx()?;
        let partial = state6.construct_tx_partial_refund()?.txid();
        let reclaim = state6.construct_tx_amnesty()?.txid();
        let chain = ScriptedChain::new(&[(partial, confirmations(9)), (reclaim, confirmations(1))]);

        let outcome = crate::protocol_v5::bob::finish_refund(
            &database, swap_id, &state6, &chain, refund_type, partial, &|_| {},
        )
        .await?;

        assert_eq!(outcome, RefundOutcome::DepositReturned(reclaim));
        assert!(matches!(
            stored(&database, swap_id).await?,
            Some(db::bob::Bob::Done(db::bob::BobEndState::BtcReclaimConfirmed(_)))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn a_confirmed_full_refund_is_done() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_with_deposit(Amount::ZERO).await?;
        let (transaction, refund_type) = state6.construct_best_bitcoin_refund_tx()?;
        assert!(matches!(refund_type, machine::bob::RefundType::Full));
        let txid = transaction.compute_txid();
        let chain = ScriptedChain::new(&[]);

        let outcome = crate::protocol_v5::bob::finish_refund(
            &database, swap_id, &state6, &chain, refund_type, txid, &|_| {},
        )
        .await?;

        assert_eq!(outcome, RefundOutcome::Refunded(txid));
        assert!(matches!(
            stored(&database, swap_id).await?,
            Some(db::bob::Bob::Done(db::bob::BobEndState::BtcRefunded(_)))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn before_the_timelock_nothing_is_sent() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_with_deposit(DEPOSIT).await?;
        let partial = state6.construct_tx_partial_refund()?.txid();
        let timelock = u32::from(state6.remaining_refund_timelock.expect("deposit timelock"));
        let chain = ScriptedChain::new(&[(partial, confirmations(timelock - 1))]).accepting();

        assert!(run_for(2, &database, swap_id, &state6, &chain).await.is_none());
        assert!(chain.broadcasts.lock().unwrap().is_empty());
        assert!(matches!(
            stored(&database, swap_id).await?,
            Some(db::bob::Bob::WaitingForReclaimTimelockExpiration(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn after_the_timelock_the_reclaim_is_broadcast() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_with_deposit(DEPOSIT).await?;
        let partial = state6.construct_tx_partial_refund()?.txid();
        // exactly the swap's deposit timelock (5 blocks in the regtest config used here)
        let timelock = u32::from(state6.remaining_refund_timelock.expect("deposit timelock"));
        let chain = ScriptedChain::new(&[(partial, confirmations(timelock))]).accepting();

        assert!(run_for(2, &database, swap_id, &state6, &chain).await.is_none());
        assert_eq!(chain.broadcasts.lock().unwrap().as_slice(), &["reclaim".to_string()]);
        assert!(matches!(
            stored(&database, swap_id).await?,
            Some(db::bob::Bob::BtcReclaimPublished(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn a_confirmed_reclaim_returns_the_deposit() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_with_deposit(DEPOSIT).await?;
        let partial = state6.construct_tx_partial_refund()?.txid();
        let reclaim = state6.construct_tx_amnesty()?.txid();
        let chain = ScriptedChain::new(&[(partial, confirmations(9)), (reclaim, confirmations(1))]);

        let outcome = run_for(5, &database, swap_id, &state6, &chain).await.expect("ends at once")?;

        assert_eq!(outcome, RefundOutcome::DepositReturned(reclaim));
        assert!(matches!(
            stored(&database, swap_id).await?,
            Some(db::bob::Bob::Done(db::bob::BobEndState::BtcReclaimConfirmed(_)))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn a_confirmed_withhold_means_the_seller_kept_the_deposit() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_with_deposit(DEPOSIT).await?;
        let partial = state6.construct_tx_partial_refund()?.txid();
        let withhold = state6.construct_tx_withhold()?.txid();
        let chain = ScriptedChain::new(&[(partial, confirmations(9)), (withhold, confirmations(1))]);

        let outcome = run_for(5, &database, swap_id, &state6, &chain).await.expect("ends at once")?;

        assert_eq!(
            outcome,
            RefundOutcome::DepositWithheld {
                withhold_txid: withhold,
                deposit: DEPOSIT,
                lock_amount: state6.tx_lock.lock_amount(),
            }
        );
        assert!(chain.broadcasts.lock().unwrap().is_empty());
        assert!(matches!(stored(&database, swap_id).await?, Some(db::bob::Bob::BtcWithheld(_))));
        Ok(())
    }

    #[tokio::test]
    async fn a_withhold_in_the_mempool_holds_back_the_reclaim() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_with_deposit(DEPOSIT).await?;
        let partial = state6.construct_tx_partial_refund()?.txid();
        let withhold = state6.construct_tx_withhold()?.txid();
        let chain =
            ScriptedChain::new(&[(partial, confirmations(9)), (withhold, ScriptStatus::InMempool)])
                .accepting();

        assert!(run_for(2, &database, swap_id, &state6, &chain).await.is_none());
        assert!(chain.broadcasts.lock().unwrap().is_empty());
        assert!(matches!(
            stored(&database, swap_id).await?,
            Some(db::bob::Bob::BtcWithholdPublished(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn the_sellers_mercy_returns_the_deposit() -> Result<()> {
        let (state6, database, swap_id, _dir) = state6_with_deposit(DEPOSIT).await?;
        let partial = state6.construct_tx_partial_refund()?.txid();
        let withhold = state6.construct_tx_withhold()?.txid();
        let mercy = state6.construct_tx_mercy()?.txid();
        let chain = ScriptedChain::new(&[
            (partial, confirmations(9)),
            (withhold, confirmations(5)),
            (mercy, confirmations(1)),
        ]);

        let outcome = run_for(5, &database, swap_id, &state6, &chain).await.expect("ends at once")?;

        assert_eq!(outcome, RefundOutcome::DepositReturned(mercy));
        assert!(matches!(
            stored(&database, swap_id).await?,
            Some(db::bob::Bob::Done(db::bob::BobEndState::BtcMercyConfirmed(_)))
        ));
        Ok(())
    }
}
