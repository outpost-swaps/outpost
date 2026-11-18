use anyhow::{Context, Result};
use std::env;
use url::Url;

#[tokio::main]
async fn main() -> Result<()> {
    swap::util::install_default_crypto_provider();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt().with_env_filter("debug").finish(),
    )?;

    let args: Vec<_> = env::args().collect();
    let proxy_index = args.iter().position(|arg| arg == "--proxy");
    let port: u16 = match proxy_index.and_then(|index| args.get(index + 1)) {
        Some(port) => port.parse()?,
        None => anyhow::bail!("usage: price_ticker --proxy <tor-socks5-port>"),
    };
    if port == 0 {
        anyhow::bail!("a valid Tor SOCKS5 proxy port is required");
    }
    let ws_str = "ws://7e6egbawekbkxzkv4244pqeqgoo4axko2imgjbedwnn6s5yb6b7oliqd.onion/ws";
    tracing::debug!(%ws_str, "Using Feather websocket url");
    let price_ticker_ws_url = Url::parse(ws_str)?;
    let primary_url = Url::parse(swap::kraken::MONEROPRICE_NOW_URL)?;
    let mut ticker = swap::kraken::connect_with_fallback(primary_url, price_ticker_ws_url, port)
        .context(
            "Failed to start price ticker (primary: moneropricenow, fallback: Feather websocket)",
        )?;

    loop {
        match ticker.wait_for_next_update().await? {
            Ok(update) => tracing::info!(%update.ask, "PRICE_TICKER_UPDATE"),
            Err(err) => tracing::info!(%err, "PRICE_TICKER_ERROR"),
        }
    }
}
