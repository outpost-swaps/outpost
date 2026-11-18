use anyhow::{anyhow, Context, Result};
use futures::stream::BoxStream;
use futures::{SinkExt, StreamExt, TryStreamExt};
use serde::Deserialize;
use std::convert::{Infallible, TryFrom};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use url::Url;

/// Primary price source: MoneroPriceNow's edge-served JSON API. Returns a
/// single XMR/BTC mid price, updated every ~30s. Routed through the Tor SOCKS
/// proxy when one is configured, like the websocket feed.
pub const MONEROPRICE_NOW_URL: &str = "https://api.moneropricenow.com/xmr-btc.json";

/// How often the primary HTTP source is polled.
const HTTP_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// If the primary source has not produced a successful update for this long,
/// the websocket fallback feed takes over until the primary recovers.
const PRIMARY_STALE_AFTER: Duration = Duration::from_secs(90);

/// If the websocket fallback has not delivered an update for this long while
/// the primary source is stale, the REST fallback starts being polled.
const WS_FALLBACK_STALE_AFTER: Duration = Duration::from_secs(120);

/// Tertiary price source: Kraken's public REST ticker for XMR/BTC. Only polled
/// when both the primary source and the websocket fallback are stale. Routed
/// through the Tor SOCKS proxy when one is configured.
pub const KRAKEN_REST_TICKER_URL: &str = "https://api.kraken.com/0/public/Ticker?pair=XMRBTC";

/// Updates older than this are considered stale and are not served to
/// consumers such as the ASB's quoting logic. Sized to survive brief feed
/// outages while limiting how long a frozen price can be quoted.
const MAX_UPDATE_AGE: Duration = Duration::from_secs(15 * 60);

/// Connect to Kraken websocket API for a constant stream of rate updates.
///
/// If the connection fails, it will automatically be re-established.
///
/// price_ticker_ws_url must point to a websocket server that follows the kraken
/// price ticker protocol
/// See: https://docs.kraken.com/websockets/
pub fn connect(price_ticker_ws_url: Url, tor_socks5_port: u16) -> Result<PriceUpdates> {
    let (price_update, price_update_receiver) = watch::channel(stamp(Err(Error::NotYetAvailable)));
    let price_update = Arc::new(price_update);

    tokio::spawn(async move {
        // The default backoff config is fine for us apart from one thing:
        // `max_elapsed_time`. If we don't get an error within this timeframe,
        // backoff won't actually retry the operation.
        let backoff = backoff::ExponentialBackoff {
            max_elapsed_time: None,
            ..backoff::ExponentialBackoff::default()
        };

        let result = backoff::future::retry_notify::<Infallible, _, _, _, _, _>(
            backoff,
            || {
                let price_update = price_update.clone();
                let price_ticker_ws_url = price_ticker_ws_url.clone();
                async move {
                    let mut stream = connection::new(price_ticker_ws_url, tor_socks5_port).await?;

                    while let Some(update) = stream.try_next().await.map_err(to_backoff)? {
                        let send_result = price_update.send(stamp(Ok(update)));

                        if send_result.is_err() {
                            return Err(backoff::Error::Permanent(anyhow!(
                                "receiver disconnected"
                            )));
                        }
                    }

                    Err(backoff::Error::transient(anyhow!("stream ended")))
                }
            },
            |error, next: Duration| {
                tracing::debug!(
                    "Kraken websocket connection failed, retrying in {}ms. Error {:#}",
                    next.as_millis(),
                    error
                );
            },
        )
        .await;

        match result {
            Err(e) => {
                tracing::warn!("Rate updates incurred an unrecoverable error: {:#}", e);

                // in case the retries fail permanently, let the subscribers know
                price_update.send(stamp(Err(Error::PermanentFailure)))
            }
            Ok(never) => match never {},
        }
    });

    Ok(PriceUpdates {
        inner: price_update_receiver,
    })
}

/// Rate updates with a resilient primary/fallback chain.
///
/// The primary source is polled over HTTP every [`HTTP_POLL_INTERVAL`]; the
/// websocket feed (Kraken protocol, e.g. Feather's ticker) acts as a fallback
/// that only publishes while the primary is stale or has never succeeded. If
/// the websocket fallback also goes silent, Kraken's REST ticker is polled as
/// a last resort. All sources are routed through the Tor SOCKS proxy when one
/// is configured.
pub fn connect_with_fallback(
    primary_http_url: Url,
    fallback_ws_url: Url,
    tor_socks5_port: u16,
) -> Result<PriceUpdates> {
    let kraken_rest_url = Url::parse(KRAKEN_REST_TICKER_URL)
        .context("Failed to parse Kraken REST ticker URL")?;

    connect_with_fallback_sources(
        primary_http_url,
        fallback_ws_url,
        kraken_rest_url,
        tor_socks5_port,
    )
}

fn connect_with_fallback_sources(
    primary_http_url: Url,
    fallback_ws_url: Url,
    kraken_rest_url: Url,
    tor_socks5_port: u16,
) -> Result<PriceUpdates> {
    let (price_update, price_update_receiver) = watch::channel(stamp(Err(Error::NotYetAvailable)));
    let price_update = Arc::new(price_update);

    tokio::spawn(async move {
        let mut poll_tick = tokio::time::interval(HTTP_POLL_INTERVAL);
        poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let mut ws_stream: Option<BoxStream<'static, Result<wire::PriceUpdate, connection::Error>>> =
            None;
        let mut last_primary_ok: Option<Instant> = None;
        let mut last_ws_ok: Option<Instant> = None;
        let mut kraken_fallback_active = false;

        loop {
            let primary_is_stale = last_primary_ok
                .map_or(true, |t| t.elapsed() > PRIMARY_STALE_AFTER);
            let ws_is_stale = last_ws_ok.map_or(true, |t| t.elapsed() > WS_FALLBACK_STALE_AFTER);

            if primary_is_stale && ws_stream.is_none() {
                match connection::new(fallback_ws_url.clone(), tor_socks5_port).await {
                    Ok(stream) => {
                        tracing::info!(
                            "Primary price source stale — using Feather websocket fallback"
                        );
                        ws_stream = Some(stream);
                    }
                    Err(e) => {
                        tracing::debug!("Feather websocket fallback failed to connect: {e:#}");
                    }
                }
            }

            let ws_tick = async {
                match ws_stream.as_mut() {
                    Some(stream) => stream.try_next().await,
                    None => std::future::pending().await,
                }
            };

            tokio::select! {
                _ = poll_tick.tick() => {
                    match poll_primary_price(&primary_http_url, tor_socks5_port).await {
                        Ok(update) => {
                            if price_update.send(stamp(Ok(update))).is_err() {
                                return;
                            }
                            last_primary_ok = Some(Instant::now());
                            kraken_fallback_active = false;
                        }
                        Err(e) => {
                            tracing::debug!("Primary price source failed: {e:#}");

                            if primary_is_stale && ws_is_stale {
                                match poll_kraken_price(&kraken_rest_url, tor_socks5_port).await {
                                    Ok(update) => {
                                        if !kraken_fallback_active {
                                            tracing::info!(
                                                "Primary source and websocket fallback stale — using Kraken REST fallback"
                                            );
                                            kraken_fallback_active = true;
                                        }
                                        if price_update.send(stamp(Ok(update))).is_err() {
                                            return;
                                        }
                                    }
                                    Err(e) => {
                                        kraken_fallback_active = false;
                                        tracing::debug!("Kraken REST fallback failed: {e:#}");
                                    }
                                }
                            }
                        }
                    }
                }
                ws_update = ws_tick, if primary_is_stale => {
                    match ws_update {
                        Ok(Some(update)) => {
                            if price_update.send(stamp(Ok(update))).is_err() {
                                return;
                            }
                            last_ws_ok = Some(Instant::now());
                            kraken_fallback_active = false;
                        }
                        _ => {
                            // Stream ended or errored — reconnect on a later pass.
                            ws_stream = None;
                        }
                    }
                }
            }
        }
    });

    Ok(PriceUpdates {
        inner: price_update_receiver,
    })
}

async fn poll_primary_price(primary_http_url: &Url, tor_socks5_port: u16) -> Result<wire::PriceUpdate> {
    let response = http_client(primary_http_url, tor_socks5_port)?
        .get(primary_http_url.clone())
        .send()
        .await
        .context("Failed to fetch primary price source")?
        .error_for_status()
        .context("Primary price source returned an error status")?;

    let body: serde_json::Value = response
        .json()
        .await
        .context("Failed to parse primary price source response")?;

    parse_moneropricenow_price(&body)
}

fn http_client(destination: &Url, tor_socks5_port: u16) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(10));
    if let Some(proxy) = bitcoin_wallet::tor_policy::socks_proxy_for(tor_socks5_port, destination)? {
        builder = builder.proxy(
            reqwest::Proxy::all(proxy).context("Failed to parse Tor SOCKS proxy URL")?,
        );
    }
    builder.build().context("Failed to build HTTP client")
}

async fn poll_kraken_price(kraken_rest_url: &Url, tor_socks5_port: u16) -> Result<wire::PriceUpdate> {
    let response = http_client(kraken_rest_url, tor_socks5_port)?
        .get(kraken_rest_url.clone())
        .send()
        .await
        .context("Failed to fetch Kraken ticker")?
        .error_for_status()
        .context("Kraken ticker returned an error status")?;

    let body: serde_json::Value = response
        .json()
        .await
        .context("Failed to parse Kraken ticker response")?;

    parse_kraken_price(&body)
}

/// Maps Kraken's `/0/public/Ticker` response
/// (`{"error":[],"result":{"XXMRXXBT":{"c":["<last>","<lots>"],...}}}`) to a
/// `PriceUpdate` denominated in sats per XMR. The pair key varies by Kraken's
/// asset naming (e.g. `XMRBTC`/`XXMRXXBT`), so the single result entry is
/// taken as-is.
fn parse_kraken_price(body: &serde_json::Value) -> Result<wire::PriceUpdate> {
    if let Some(errors) = body["error"].as_array() {
        if !errors.is_empty() {
            anyhow::bail!("Kraken ticker returned API errors: {errors:?}");
        }
    }

    let result = body["result"]
        .as_object()
        .context("Kraken ticker response is missing the `result` field")?;
    let pair = result
        .values()
        .next()
        .context("Kraken ticker response contained no pair data")?;
    let last_trade = pair["c"][0]
        .as_str()
        .context("Kraken ticker response is missing the last-trade price")?;
    let btc_per_xmr: f64 = last_trade
        .parse()
        .context("Failed to parse Kraken last-trade price")?;
    if !btc_per_xmr.is_finite() || btc_per_xmr <= 0.0 {
        anyhow::bail!("Kraken ticker returned an implausible price: {btc_per_xmr}");
    }

    let sats_per_xmr = (btc_per_xmr * 100_000_000.0).round() as u64;
    Ok(wire::PriceUpdate {
        ask: bitcoin::Amount::from_sat(sats_per_xmr),
    })
}

/// Maps MoneroPriceNow's `{"pair":"XMR/BTC","price":<btc_per_xmr>,...}` to a
/// `PriceUpdate` denominated in sats per XMR.
fn parse_moneropricenow_price(body: &serde_json::Value) -> Result<wire::PriceUpdate> {
    let btc_per_xmr = body["price"]
        .as_f64()
        .context("Primary price source response is missing the `price` field")?;
    if !btc_per_xmr.is_finite() || btc_per_xmr <= 0.0 {
        anyhow::bail!("Primary price source returned an implausible price: {btc_per_xmr}");
    }
    let sats_per_xmr = (btc_per_xmr * 100_000_000.0).round() as u64;
    Ok(wire::PriceUpdate {
        ask: bitcoin::Amount::from_sat(sats_per_xmr),
    })
}

/// A price update paired with its receive time, so consumers can reject
/// stale prices instead of quoting on a frozen feed.
#[derive(Clone, Debug)]
struct StampedPriceUpdate {
    update: PriceUpdate,
    received_at: Instant,
}

fn stamp(update: PriceUpdate) -> StampedPriceUpdate {
    StampedPriceUpdate {
        update,
        received_at: Instant::now(),
    }
}

#[derive(Clone, Debug)]
pub struct PriceUpdates {
    inner: watch::Receiver<StampedPriceUpdate>,
}

impl PriceUpdates {
    pub async fn wait_for_next_update(&mut self) -> Result<PriceUpdate> {
        self.inner.changed().await?;

        Ok(self.inner.borrow().update.clone())
    }

    /// Returns the latest price update, or an error if the newest update is
    /// older than `MAX_UPDATE_AGE` (or the newest update itself is an error
    /// value). Consumers see this as the feed being unavailable, so they fail
    /// closed rather than serving a frozen price.
    pub fn latest_update(&mut self) -> PriceUpdate {
        let stamped = self.inner.borrow().clone();
        let age = stamped.received_at.elapsed();
        if age > MAX_UPDATE_AGE {
            return Err(Error::Stale {
                age_secs: age.as_secs(),
            });
        }

        stamped.update
    }
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
    #[error("Rate is not yet available")]
    NotYetAvailable,
    #[error("Permanently failed to retrieve rate from Kraken")]
    PermanentFailure,
    #[error("Price update is stale ({age_secs}s old)")]
    Stale { age_secs: u64 },
}

type PriceUpdate = Result<wire::PriceUpdate, Error>;

/// Maps a [`connection::Error`] to a backoff error, effectively defining our
/// retry strategy.
fn to_backoff(e: connection::Error) -> backoff::Error<anyhow::Error> {
    use backoff::Error::*;

    match e {
        // Connection closures and websocket errors will be retried
        connection::Error::ConnectionClosed => backoff::Error::transient(anyhow::Error::from(e)),
        connection::Error::WebSocket(_) => backoff::Error::transient(anyhow::Error::from(e)),

        // Failures while parsing a message are permanent because they most likely present a
        // programmer error
        connection::Error::Parse(_) => Permanent(anyhow::Error::from(e)),
    }
}

/// Kraken websocket connection module.
///
/// Responsible for establishing a connection to the Kraken websocket API and
/// transforming the received websocket frames into a stream of rate updates.
/// The connection may fail in which case it is simply terminated and the stream
/// ends.
mod connection {
    use std::net::Ipv4Addr;
    use super::*;
    use crate::kraken::wire;
    use futures::stream::BoxStream;
    use tokio::io;
    use tokio::net::TcpStream;
    use tokio_socks::tcp::Socks5Stream;
    use tokio_tungstenite::{MaybeTlsStream, tungstenite, WebSocketStream};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::error::UrlError;
    use tokio_tungstenite::tungstenite::handshake::client::Response;
    use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
    use tungstenite::Error as WsError;

    pub async fn new(ws_url: Url, tor_socks5_port: u16) -> Result<BoxStream<'static, Result<wire::PriceUpdate, Error>>> {
        let (mut rate_stream, _) = connect_async_with_config(ws_url.as_str(), None, tor_socks5_port)
            .await
            .context("Failed to connect to Kraken websocket API")?;

        let stream = rate_stream.err_into().try_filter_map(parse_message).boxed();

        Ok(stream)
    }

    /// Parse a websocket message into a [`wire::PriceUpdate`].
    ///
    /// Messages which are not actually ticker updates are ignored and result in
    /// `None` being returned. In the context of a [`TryStream`], these will
    /// simply be filtered out.
    async fn parse_message(msg: tungstenite::Message) -> Result<Option<wire::PriceUpdate>, Error> {

        let msg = match msg {
            tungstenite::Message::Text(msg) => msg.to_string(),
            tungstenite::Message::Close(close_frame) => {
                if let Some(tungstenite::protocol::CloseFrame { code, reason }) = close_frame {
                    tracing::debug!(
                        "Kraken rate stream was closed with code {} and reason: {}",
                        code,
                        reason
                    );
                } else {
                    tracing::debug!("Kraken rate stream was closed without code and reason");
                }

                return Err(Error::ConnectionClosed);
            }
            msg => msg.to_string()
        };

        return match serde_json::from_str::<wire::Event>(&msg) {
            Ok(wire::Event::CryptoRates) => {
                let prices = match serde_json::from_str::<wire::PriceUpdate>(&msg) {
                    Ok(ticker) => {
                        tracing::debug!("Updated coin prices");
                        Some(ticker)
                    },
                    Err(error) => {
                        tracing::trace!(%msg, "Failed to deserialize message as ticker update. Error {:#}", error);
                        None
                    }
                };
                Ok(prices)
            }
            // if the message is not an event, it is a ticker update or an unknown event
            Err(error) => {
                tracing::trace!(%msg, "Failed to deserialize message as ticker update. Error {:#}", error);
                Ok(None)
            },
        };
    }

    #[derive(Debug, thiserror::Error)]
    pub enum Error {
        #[error("The Kraken server closed the websocket connection")]
        ConnectionClosed,
        #[error("Failed to read message from websocket stream")]
        WebSocket(#[from] tungstenite::Error),
        #[error("Failed to parse rate from websocket message")]
        Parse(#[from] wire::Error),
    }

    const SUBSCRIBE_XMR_BTC_TICKER_PAYLOAD: &str = r#"
    { "event": "subscribe",
      "pair": [ "XMR/XBT" ],
      "subscription": {
        "name": "ticker"
      }
    }"#;

    fn domain(request: &tungstenite::handshake::client::Request) -> Result<String, WsError> {
        match request.uri().host() {
            Some(d) => Ok(d.to_string()),
            None => Err(WsError::Url(tungstenite::error::UrlError::NoHostName)),
        }
    }

    pub async fn connect_async_with_config<R>(
        request: R,
        config: Option<WebSocketConfig>,
        tor_socks5_port: u16
    ) -> Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response), WsError>
        where
            R: IntoClientRequest + Unpin,
    {
        let request = request.into_client_request()?;

        let domain = domain(&request)?;
        let port = request
            .uri()
            .port_u16()
            .or_else(|| match request.uri().scheme_str() {
                Some("wss") => Some(443),
                Some("ws") => Some(80),
                _ => None,
            })
            .ok_or(WsError::Url(UrlError::UnsupportedUrlScheme))?;

        let addr = format!("{}:{}", domain, port);
        let stream =
            Socks5Stream::connect((Ipv4Addr::LOCALHOST, tor_socks5_port), addr.to_string())
                .await
                .map_err(|e| io::Error::new(io::ErrorKind::ConnectionRefused, e))?;
        let try_socket = Ok(stream.into_inner());
        let socket = try_socket.map_err(WsError::Io)?;

        tokio_tungstenite::client_async_tls_with_config(request, socket, config, None).await
    }
}

/// Kraken websocket API wire module.
///
/// Responsible for parsing websocket text messages to events and rate updates.
mod wire {
    use bitcoin::Amount;
    use super::*;
    use bitcoin::amount::ParseAmountError;

    #[derive(Debug, Deserialize, PartialEq, Eq)]
    #[serde(tag = "cmd")]
    pub enum Event {
        #[serde(rename = "crypto_rates")]
        CryptoRates,
    }

    #[derive(Clone, Debug, thiserror::Error)]
    pub enum Error {
        #[error("Data field is missing")]
        DataFieldMissing,
        #[error("Ask Rate Element is of unexpected type")]
        UnexpectedAskRateElementType,
        #[error("Ask Rate Element is missing")]
        MissingAskRateElementType,
        #[error("Failed to parse Bitcoin amount")]
        BitcoinParseAmount(#[from] ParseAmountError),
    }

    /// Represents an update within the price ticker.
    #[derive(Clone, Debug, Deserialize)]
    #[serde(try_from = "TickerData")]
    pub struct PriceUpdate {
        pub ask: bitcoin::Amount,
    }

    #[derive(Debug, Deserialize)]
    pub struct TickerData {
        #[serde(rename = "data")]
        coins: Vec<CoinData>,
    }

    #[derive(Debug, Deserialize)]
    pub struct CoinData {
        #[serde(rename = "symbol")]
        ticker: String,
        #[serde(rename = "current_price")]
        price: f64,
    }

    #[derive(Debug, Deserialize)]
    #[serde(untagged)]
    pub enum RateElement {
        Text(String),
        Number(u64),
    }

    impl TryFrom<TickerData> for PriceUpdate {
        type Error = Error;

        fn try_from(value: TickerData) -> Result<Self, Error> {
            let _monero_ticker = "xmr".to_string();
            let _bitcoin_ticker = "btc".to_string();
            let mut xmr_price = 0f64;
            let mut btc_price = 0f64;
            for coin_data in &value.coins {
                if coin_data.ticker == _monero_ticker {
                    xmr_price = coin_data.price;
                }
                if coin_data.ticker == _bitcoin_ticker {
                    btc_price = coin_data.price;
                }
            }
            let sats_per_xmr = xmr_price / btc_price;
            let final_sats_per_xmr = (sats_per_xmr * 100000000.0).round() / 100000000.0;
            Ok(PriceUpdate { ask: Amount::from_btc(final_sats_per_xmr)? })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moneropricenow_response_parses_to_sats_per_xmr() {
        let body: serde_json::Value =
            serde_json::from_str(r#"{"pair":"XMR/BTC","price":0.006291,"published_utc":"2026-10-10T22:38:07Z","source":"moneropricenow.com"}"#).unwrap();
        let update = parse_moneropricenow_price(&body).unwrap();
        assert_eq!(update.ask, bitcoin::Amount::from_sat(629_100));
    }

    #[test]
    fn kraken_ticker_response_parses_to_sats_per_xmr() {
        let body: serde_json::Value = serde_json::from_str(
            r#"{"error":[],"result":{"XXMRXXBT":{"a":["0.00715100","1","1.000"],"b":["0.00713700","1","1.000"],"c":["0.00715300","0.02075086"]}}}"#,
        )
        .unwrap();
        let update = parse_kraken_price(&body).unwrap();
        assert_eq!(update.ask, bitcoin::Amount::from_sat(715_300));
    }

    #[test]
    fn kraken_ticker_api_errors_are_rejected() {
        let body: serde_json::Value =
            serde_json::from_str(r#"{"error":["EQuery:Unknown asset pair"],"result":{}}"#).unwrap();
        assert!(parse_kraken_price(&body).is_err());
    }

    #[test]
    fn latest_update_rejects_stale_prices() {
        let (tx, rx) = watch::channel(stamp(Ok(wire::PriceUpdate {
            ask: bitcoin::Amount::from_sat(629_100),
        })));
        let mut updates = PriceUpdates { inner: rx };

        assert!(updates.latest_update().is_ok());

        let _ = tx.send(StampedPriceUpdate {
            update: Ok(wire::PriceUpdate {
                ask: bitcoin::Amount::from_sat(629_100),
            }),
            received_at: Instant::now() - MAX_UPDATE_AGE - Duration::from_secs(1),
        });

        match updates.latest_update() {
            Err(Error::Stale { .. }) => {}
            other => panic!("expected stale error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn primary_http_source_feeds_updates_before_websocket_fallback() {
        // Minimal HTTP server speaking MoneroPriceNow's JSON shape.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let body = r#"{"pair":"XMR/BTC","price":0.006291,"published_utc":"2026-10-10T22:38:07Z","source":"moneropricenow.com"}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        // Fallback websocket points at a dead port — the update must come from
        // the primary HTTP source regardless.
        let primary = Url::parse(&format!("http://127.0.0.1:{port}/xmr-btc.json")).unwrap();
        let fallback = Url::parse("ws://127.0.0.1:1/ws").unwrap();
        let mut updates = connect_with_fallback(primary, fallback, 0).unwrap();

        let update = tokio::time::timeout(Duration::from_secs(10), updates.wait_for_next_update())
            .await
            .expect("timed out waiting for the primary price update")
            .expect("price update transport errored")
            .expect("price update was an error value");
        assert_eq!(update.ask, bitcoin::Amount::from_sat(629_100));
    }

    #[tokio::test]
    async fn kraken_rest_feeds_updates_when_primary_and_websocket_are_dead() {
        // Minimal HTTP server speaking Kraken's Ticker shape.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 2048];
                let _ = stream.read(&mut buf);
                let body = r#"{"error":[],"result":{"XXMRXXBT":{"c":["0.00715300","1.0"]}}}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        // Both the primary source and the websocket fallback are unreachable;
        // the update must arrive via the Kraken REST fallback.
        let primary = Url::parse("http://127.0.0.1:1/xmr-btc.json").unwrap();
        let fallback = Url::parse("ws://127.0.0.1:1/ws").unwrap();
        let kraken = Url::parse(&format!("http://127.0.0.1:{port}/0/public/Ticker")).unwrap();
        let mut updates =
            connect_with_fallback_sources(primary, fallback, kraken, 0).unwrap();

        let update = tokio::time::timeout(Duration::from_secs(15), updates.wait_for_next_update())
            .await
            .expect("timed out waiting for the Kraken fallback update")
            .expect("price update transport errored")
            .expect("price update was an error value");
        assert_eq!(update.ask, bitcoin::Amount::from_sat(715_300));
    }
}

#[cfg(test)]
mod tor_required_tests {
    use super::*;

    #[test]
    fn a_remote_price_source_without_a_tor_port_is_refused() {
        let url = Url::parse("https://api.example.test/xmr-btc.json").unwrap();
        assert!(http_client(&url, 0).is_err());
    }

    #[test]
    fn a_remote_price_source_with_a_tor_port_builds() {
        let url = Url::parse("https://api.example.test/xmr-btc.json").unwrap();
        assert!(http_client(&url, 9050).is_ok());
    }
}

