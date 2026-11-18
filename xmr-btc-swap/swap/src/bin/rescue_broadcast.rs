use anyhow::{bail, Context, Result};
use electrum_client::{Client as ElectrumClient, ConfigBuilder, ElectrumApi, Socks5Config};
use swap::bitcoin::Transaction;

/// One-off rescue tool: broadcast a transaction (the JSON shape stored in the
/// swap database) through a chosen Electrum server over a SOCKS5 (Tor) proxy.
/// Used when the primary Electrum server has a transaction stuck in its mempool
/// and not propagating to miners.
///
/// usage: rescue_broadcast <tx.json> <electrum-url> <socks5-proxy>
fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: rescue_broadcast <tx.json> <electrum-url> <socks5-proxy>")?;
    let url = std::env::args()
        .nth(2)
        .context("electrum url (tcp://host:port)")?;
    let socks = std::env::args()
        .nth(3)
        .context("a Tor SOCKS5 proxy is required")?;
    if socks.is_empty() || socks.ends_with(":0") {
        bail!("a valid Tor SOCKS5 proxy is required");
    }

    let data = std::fs::read_to_string(&path).context("failed to read tx json")?;
    let tx: Transaction = serde_json::from_str(&data).context("failed to parse tx json")?;

    let builder = ConfigBuilder::default()
        .retry(5)
        .socks5(Some(Socks5Config::new(socks)));
    let client = ElectrumClient::from_config(&url, builder.build())
        .context("failed to initialize electrum client")?;

    let txid = client
        .transaction_broadcast(&tx)
        .context("broadcast failed")?;
    println!("BROADCAST_OK {}", txid);
    Ok(())
}
