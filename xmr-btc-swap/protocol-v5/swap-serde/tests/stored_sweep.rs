//! The signed Monero sweep is stored inside the encrypted swap state with
//! `swap_serde::monero::transaction` (matrix S-03). Round-trip a real one: a stagenet sweep
//! mined at block 2246652 (swap `01a611e8`), fetched from a public daemon.

use monero_oxide_wallet::transaction::{NotPruned, Transaction};
use serde::{Deserialize, Serialize};

const SWEEP_HEX: &str = include_str!("fixtures/stagenet_sweep_tx.hex");
const SWEEP_TXID: &str = "ed134b4707d78f224145228178539c0c76463290defef9e7ff5af1189d935644";

#[derive(Serialize, Deserialize)]
struct Stored {
    #[serde(with = "swap_serde::monero::transaction")]
    xmr_redeem_tx: Transaction<NotPruned>,
}

fn sweep() -> Transaction<NotPruned> {
    let bytes = hex::decode(SWEEP_HEX.trim()).unwrap();
    let mut reader = bytes.as_slice();
    let transaction = Transaction::<NotPruned>::read(&mut reader).unwrap();
    assert!(reader.is_empty());
    transaction
}

#[test]
fn the_fixture_is_the_sweep_it_claims_to_be() {
    assert_eq!(hex::encode(sweep().hash()), SWEEP_TXID);
}

#[test]
fn a_signed_sweep_survives_the_stored_json_form() {
    let before = sweep();
    let json = serde_json::to_string(&Stored { xmr_redeem_tx: sweep() }).unwrap();
    // human-readable formats store the wire-format hex
    assert!(json.contains(SWEEP_HEX.trim()), "stored form is the wire hex");

    let after: Stored = serde_json::from_str(&json).unwrap();
    assert_eq!(after.xmr_redeem_tx.hash(), before.hash());
    assert_eq!(after.xmr_redeem_tx.serialize(), before.serialize());
}
