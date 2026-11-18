use curve25519_dalek_ng::scalar::Scalar;
use monero_oxide_ext::{PrivateKey, PublicKey};
use rand_chacha::ChaCha20Rng;
use rand_chacha::rand_core::SeedableRng;
use sha2::{Digest, Sha256};
use swap_core::bitcoin::{self, Amount, RedeemFeeMultiplier, SecretKey, SwapFeeRate};
use swap_core::monero::PrivateViewKey;
use swap_machine_common::{CROSS_CURVE_PROOF_SYSTEM, Message0, Message1};
use uuid::Uuid;

fn key(byte: u8) -> PrivateKey {
    PrivateKey::from_slice(&[byte; 32]).unwrap()
}

fn address() -> bitcoin::Address {
    "bcrt1q08pfqpsyrt7acllzyjm8q5qsz5capvyahm49rw"
        .parse::<bitcoin::Address<::bitcoin::address::NetworkUnchecked>>()
        .unwrap()
        .assume_checked()
}

#[test]
fn message0_json_and_cbor_are_stable() {
    let mut rng = ChaCha20Rng::seed_from_u64(7);
    let b = SecretKey::new_random(&mut rng);
    let monero_secret = Scalar::random(&mut rng);
    let (proof, _) = CROSS_CURVE_PROOF_SYSTEM.prove(&monero_secret, &mut rng);

    let message = Message0 {
        swap_id: Uuid::from_u128(0x1234_5678_9abc_def0_1122_3344_5566_7788),
        B: b.public(),
        S_b_monero: PublicKey::from_private_key(&key(3)),
        S_b_bitcoin: b.public(),
        dleq_proof_s_b: proof,
        v_b: PrivateViewKey(key(5)),
        refund_address: address(),
    };

    let json = serde_json::to_string_pretty(&message).unwrap();
    let cbor = serde_cbor::to_vec(&message).unwrap();
    let json_hash = hex::encode(Sha256::digest(json.as_bytes()));
    let cbor_hash = hex::encode(Sha256::digest(&cbor));

    assert_eq!(
        json_hash,
        "e807138e2fe8f52b45b60b964a2eec6df6bcf02863c7fc47cdb259be150ad870"
    );
    assert_eq!(
        cbor_hash,
        "071c95897a97cb6181d98456d75da56619059ff1f6c1857828db63247432998a"
    );

    let decoded: Message0 = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.swap_id, message.swap_id);
    assert_eq!(decoded.refund_address, message.refund_address);
}

#[test]
fn message1_fee_fields_and_addresses_are_stable() {
    let mut rng = ChaCha20Rng::seed_from_u64(11);
    let a = SecretKey::new_random(&mut rng);
    let monero_secret = Scalar::random(&mut rng);
    let (proof, _) = CROSS_CURVE_PROOF_SYSTEM.prove(&monero_secret, &mut rng);

    let message = Message1 {
        A: a.public(),
        S_a_monero: PublicKey::from_private_key(&key(7)),
        S_a_bitcoin: a.public(),
        dleq_proof_s_a: proof,
        v_a: PrivateViewKey(key(9)),
        redeem_address: address(),
        punish_address: address(),
        fee_rate: SwapFeeRate(5_000),
        redeem_multiplier: RedeemFeeMultiplier::try_from(250).unwrap(),
        amnesty_amount: Amount::from_sat(1_234),
    };

    let json = serde_json::to_string_pretty(&message).unwrap();
    let cbor = serde_cbor::to_vec(&message).unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(json.as_bytes())),
        "de08ead4b06f4e36bfeae09073b42c109aadcb35bcf2576b5369ffbcb990ce3d"
    );
    assert_eq!(
        hex::encode(Sha256::digest(&cbor)),
        "7f7923f54c0d92af0fe8fb70b93116b9468fa4d623375bed21082ba515a1b51e"
    );

    let decoded: Message1 = serde_cbor::from_slice(&cbor).unwrap();
    assert_eq!(decoded.fee_rate, message.fee_rate);
    assert_eq!(decoded.redeem_multiplier, message.redeem_multiplier);
    assert_eq!(decoded.amnesty_amount, message.amnesty_amount);
}

#[test]
fn setup_fee_limits_match_the_5_0_2_contract() {
    use swap_core::bitcoin::{Amount, SwapFeeRate};
    use swap_machine_common::swap_fee_limits;

    let limits = swap_fee_limits(SwapFeeRate(5_000), Amount::from_sat(1_000_000)).unwrap();
    assert_eq!(limits.min_rate, SwapFeeRate(2_500));
    assert_eq!(limits.max_rate, SwapFeeRate(15_000));
    assert_eq!(limits.max_path_fee, Amount::from_sat(100_000));
}

#[test]
fn full_refund_transaction_vectors_are_stable() {
    use ::bitcoin::{absolute::LockTime, transaction::Version, OutPoint, Sequence, TxIn, TxOut, Txid};
    use rand_chacha::ChaCha20Rng;
    use rand_chacha::rand_core::SeedableRng;
    use swap_core::bitcoin::{
        build_shared_output_descriptor, CancelRefundVersion, CancelTimelock, PunishTimelock,
        RemainingRefundTimelock, SecretKey, TxCancel, TxEarlyRefund, TxFullRefund, TxLock,
        TxPartialRefund, TxPunish, TxReclaim, TxRedeem,
    };

    let mut rng = ChaCha20Rng::seed_from_u64(21);
    let a = SecretKey::new_random(&mut rng).public();
    let b = SecretKey::new_random(&mut rng).public();

    let amount = Amount::from_sat(100_000);
    let descriptor = build_shared_output_descriptor(a.into(), b.into()).unwrap();
    let tx = ::bitcoin::Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(
                Txid::from_raw_hash(::bitcoin::hashes::Hash::all_zeros()),
                0,
            ),
            script_sig: Default::default(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Default::default(),
        }],
        output: vec![TxOut {
            value: amount,
            script_pubkey: descriptor.script_pubkey(),
        }],
    };
    let mut psbt = ::bitcoin::psbt::Psbt::from_unsigned_tx(tx).unwrap();
    psbt.inputs[0].witness_utxo = Some(TxOut {
        value: amount + Amount::from_sat(2_000),
        script_pubkey: address().script_pubkey(),
    });

    let tx_lock = TxLock::from_psbt(psbt, a, b, amount).unwrap();
    let tx_cancel = TxCancel::new(
        &tx_lock,
        CancelTimelock(30),
        a,
        b,
        Amount::from_sat(500),
        CancelRefundVersion::Legacy,
    )
    .unwrap();
    let tx_refund = TxFullRefund::new(&tx_cancel, &address(), Amount::from_sat(400));
    let tx_redeem = TxRedeem::new(&tx_lock, &address(), Amount::from_sat(450));
    let tx_punish = TxPunish::new(
        &tx_cancel,
        &address(),
        PunishTimelock(60),
        Amount::from_sat(400),
    );
    let tx_partial_refund = TxPartialRefund::new(
        &tx_cancel,
        &address(),
        a,
        b,
        Amount::from_sat(1_000),
        Amount::from_sat(400),
    )
    .unwrap();
    let tx_early_refund = TxEarlyRefund::new(&tx_lock, &address(), Amount::from_sat(350));
    let tx_reclaim = TxReclaim::new(
        &tx_partial_refund,
        &address(),
        Amount::from_sat(300),
        RemainingRefundTimelock(20),
    )
    .unwrap();

    let vectors = [
        (
            tx_lock.txid().to_string(),
            "9f8fb1ff93ab26e2737aa9f23dcce43823c1738e57830b10abfd74e1f65c5ca8",
        ),
        (
            tx_cancel.txid().to_string(),
            "314d1a4cd918180eb093e0442971d3540b89eea18d5676eaded03fe2bdda3c4f",
        ),
        (
            tx_refund.txid().to_string(),
            "0087d338ed78e4d3195c946b52fd0ecd54e3a4439865c05d35ed993da41faf7d",
        ),
        (
            tx_redeem.txid().to_string(),
            "37499d9aaebb1a882a34e7719610d37bbe54358f415b1301474b6e0c3497a66d",
        ),
        (
            tx_punish.txid().to_string(),
            "4497c045083f06db5a07da0f7a7f7e6002bc9c119bd2c162ee701243452f0fcd",
        ),
        (
            tx_partial_refund.txid().to_string(),
            "5a37d769516a1d5a97f909aad081862cd565ad5fe08a73a0afb5997b7a61cf62",
        ),
        (
            tx_early_refund.txid().to_string(),
            "106f3b6092b3146ad1abeac26221c3ba0f1e455f07baa7efdd89fcfde1685b37",
        ),
        (
            tx_reclaim.txid().to_string(),
            "d03923f63df61b993a2ce60e3096b772ced0e02cc4e49d7640c838f47c6edc6b",
        ),
    ];
    for (actual, expected) in vectors {
        assert_eq!(actual, expected);
    }
}
