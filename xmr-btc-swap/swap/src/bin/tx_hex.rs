use swap::bitcoin::Transaction;

/// One-off utility: parse a serialized bitcoin::Transaction (the JSON shape
/// used in the swap database, e.g. Bob.BtcLockSigned.signed_transaction) and
/// print the raw transaction hex for external broadcasting.
fn main() {
    let path = std::env::args().nth(1).expect("usage: tx_hex <tx.json>");
    let data = std::fs::read_to_string(path).expect("failed to read tx json file");
    let tx: Transaction = serde_json::from_str(&data).expect("failed to parse tx json");
    let raw = ::bitcoin::consensus::serialize(&tx);
    println!("{}", hex::encode(raw));
}
