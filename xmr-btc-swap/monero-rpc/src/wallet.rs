use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use jsonrpc_client::{Response, SendRequest};
use md5::{Digest, Md5};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::{StatusCode, Url};
use rust_decimal::Decimal;
use serde::de::{DeserializeOwned, Error};
use serde::{Deserialize, Deserializer, Serialize};

#[jsonrpc_client::api(version = "2.0")]
pub trait MoneroWalletRpc {
    async fn get_address(&self, account_index: u32) -> GetAddress;
    async fn get_balance(&self, account_index: u32) -> GetBalance;
    async fn create_account(&self, label: String) -> CreateAccount;
    async fn get_accounts(&self, tag: String) -> GetAccounts;
    async fn open_wallet(&self, filename: String) -> WalletOpened;
    async fn close_wallet(&self) -> WalletClosed;
    async fn create_wallet(&self, filename: String, language: String) -> WalletCreated;
    async fn transfer(
        &self,
        account_index: u32,
        destinations: Vec<Destination>,
        get_tx_key: bool,
        do_not_relay: bool,
        get_tx_hex: bool,
        get_tx_metadata: bool,
    ) -> Transfer;
    async fn relay_tx(&self, hex: String) -> RelayTx;
    async fn submit_transaction(&self, tx_data_hex: String) -> SubmitTransaction;
    async fn get_height(&self) -> BlockHeight;
    async fn check_tx_key(&self, txid: String, tx_key: String, address: String) -> CheckTxKey;
    async fn get_transfer_by_txid(&self, txid: String) -> GetTransferByTxid;
    async fn get_transfers(&self, out: bool, pending: bool) -> GetTransfers;
    async fn get_tx_key(&self, txid: String) -> GetTxKey;
    #[allow(clippy::too_many_arguments)]
    async fn generate_from_keys(
        &self,
        filename: String,
        address: String,
        spendkey: String,
        viewkey: String,
        restore_height: u32,
        password: String,
        autosave_current: bool,
    ) -> GenerateFromKeys;
    async fn refresh(&self) -> Refreshed;
    async fn sweep_all(
        &self,
        address: String,
        do_not_relay: bool,
        get_tx_keys: bool,
        get_tx_hex: bool,
        get_tx_metadata: bool,
    ) -> SweepAll;
    async fn get_version(&self) -> Version;
    async fn store(&self);
}

#[jsonrpc_client::implement(MoneroWalletRpc)]
#[derive(Debug, Clone)]
pub struct Client {
    inner: DigestClient,
    base_url: reqwest::Url,
}

#[derive(Debug, Clone)]
pub struct DigestClient {
    inner: reqwest::Client,
    username: String,
    password: String,
    nonce_count: Arc<AtomicU32>,
}

#[derive(Debug)]
struct DigestChallenge {
    realm: String,
    nonce: String,
    opaque: Option<String>,
    algorithm: String,
    qop: Option<String>,
}

#[async_trait::async_trait]
impl SendRequest for DigestClient {
    type Error = reqwest::Error;

    async fn send_request<P>(&self, endpoint: Url, body: String) -> Result<Response<P>, Self::Error>
    where
        P: DeserializeOwned,
    {
        let request = || {
            self.inner
                .post(endpoint.clone())
                .header(CONTENT_TYPE, "application/json")
                .body(body.clone())
        };

        let response = request().send().await?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return response.json().await;
        }

        let challenge = response
            .headers()
            .get_all(WWW_AUTHENTICATE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(parse_digest_challenge)
            .ok_or_else(|| response.error_for_status_ref().unwrap_err())?;

        let uri = match endpoint.query() {
            Some(query) => format!("{}?{}", endpoint.path(), query),
            None => endpoint.path().to_owned(),
        };
        let authorization = self
            .authorization(&challenge, "POST", &uri)
            .ok_or_else(|| response.error_for_status_ref().unwrap_err())?;

        request()
            .header(AUTHORIZATION, authorization)
            .send()
            .await?
            .json()
            .await
    }
}

impl DigestClient {
    fn authorization(
        &self,
        challenge: &DigestChallenge,
        method: &str,
        uri: &str,
    ) -> Option<String> {
        let algorithm = challenge.algorithm.to_ascii_uppercase();
        let qop = challenge
            .qop
            .as_deref()
            .and_then(|qop| qop.split(',').map(str::trim).find(|qop| *qop == "auth"));
        if challenge.qop.is_some() && qop.is_none() {
            return None;
        }

        let mut ha1 = md5_hex(&format!(
            "{}:{}:{}",
            self.username, challenge.realm, self.password
        ));
        let cnonce = cnonce();
        if algorithm == "MD5-SESS" {
            ha1 = md5_hex(&format!("{}:{}:{}", ha1, challenge.nonce, cnonce));
        }

        let ha2 = md5_hex(&format!("{}:{}", method, uri));
        let nc = format!(
            "{:08x}",
            self.nonce_count.fetch_add(1, Ordering::SeqCst) + 1
        );

        let response = match qop {
            Some(qop) => md5_hex(&format!(
                "{}:{}:{}:{}:{}:{}",
                ha1, challenge.nonce, nc, cnonce, qop, ha2
            )),
            None => md5_hex(&format!("{}:{}:{}", ha1, challenge.nonce, ha2)),
        };

        let mut fields = vec![
            format!("username=\"{}\"", escape(&self.username)),
            format!("realm=\"{}\"", escape(&challenge.realm)),
            format!("nonce=\"{}\"", escape(&challenge.nonce)),
            format!("uri=\"{}\"", escape(uri)),
            format!("response=\"{}\"", response),
            format!("algorithm={}", challenge.algorithm),
        ];
        if let Some(opaque) = &challenge.opaque {
            fields.push(format!("opaque=\"{}\"", escape(opaque)));
        }
        if let Some(qop) = qop {
            fields.push(format!("qop={}", qop));
            fields.push(format!("nc={}", nc));
            fields.push(format!("cnonce=\"{}\"", cnonce));
        }

        Some(format!("Digest {}", fields.join(", ")))
    }
}

fn parse_digest_challenge(header: &str) -> Option<DigestChallenge> {
    let params = header.trim().strip_prefix("Digest ")?;
    let mut values = HashMap::new();
    for part in params.split(',') {
        let (key, value) = part.split_once('=')?;
        values.insert(
            key.trim().to_ascii_lowercase(),
            value.trim().trim_matches('"').to_owned(),
        );
    }

    Some(DigestChallenge {
        realm: values.remove("realm")?,
        nonce: values.remove("nonce")?,
        opaque: values.remove("opaque"),
        algorithm: values
            .remove("algorithm")
            .unwrap_or_else(|| "MD5".to_owned()),
        qop: values.remove("qop"),
    })
}

fn md5_hex(value: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

fn cnonce() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

impl Client {
    /// Constructs a monero-wallet-rpc client with localhost endpoint.
    pub fn localhost(port: u16) -> Result<Self> {
        Client::new(
            format!("http://127.0.0.1:{}/json_rpc", port)
                .parse()
                .context("url is well formed")?,
        )
    }

    /// Constructs a monero-wallet-rpc client with `url` endpoint. Credentials
    /// embedded in the URL (the GUI's local RPC starts with `--rpc-login`) are
    /// removed from the request URL and used for HTTP Digest authentication.
    pub fn new(mut url: reqwest::Url) -> Result<Self> {
        let username = url.username().to_owned();
        let password = url.password().unwrap_or_default().to_owned();
        let _ = url.set_username("");
        let _ = url.set_password(None);

        Ok(Self {
            inner: DigestClient {
                inner: reqwest::ClientBuilder::new()
                    .connection_verbose(true)
                    .build()?,
                username,
                password,
                nonce_count: Arc::new(AtomicU32::new(0)),
            },
            base_url: url,
        })
    }

    /// Transfers `amount` monero from `account_index` to `address`.
    pub async fn transfer_single(
        &self,
        account_index: u32,
        amount: u64,
        address: &str,
    ) -> Result<Transfer> {
        let dest = vec![Destination {
            amount,
            address: address.to_owned(),
        }];

        Ok(self
            .transfer(account_index, dest, true, false, false, false)
            .await?)
    }

    /// Construct a multi-destination transaction without relaying it.
    pub async fn transfer_constructed(
        &self,
        account_index: u32,
        destinations: Vec<Destination>,
    ) -> Result<Transfer> {
        Ok(self
            .transfer(account_index, destinations, true, true, true, true)
            .await?)
    }

    /// Relay a transaction previously constructed with `do_not_relay`.
    pub async fn relay_transaction(&self, metadata: String) -> Result<RelayTx> {
        Ok(self.relay_tx(metadata).await?)
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct GetAddress {
    pub address: String,
}

#[derive(Deserialize, Debug, Clone, Copy)]
pub struct GetBalance {
    pub balance: u64,
    pub unlocked_balance: u64,
    pub multisig_import_needed: bool,
    pub blocks_to_unlock: u32,
    pub time_to_unlock: u32,
}

impl fmt::Display for GetBalance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut total = Decimal::from(self.balance);
        total
            .set_scale(12)
            .expect("12 is smaller than max precision of 28");

        let mut unlocked = Decimal::from(self.unlocked_balance);
        unlocked
            .set_scale(12)
            .expect("12 is smaller than max precision of 28");

        write!(
            f,
            "total balance: {}, unlocked balance: {}",
            total, unlocked
        )
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct CreateAccount {
    pub account_index: u32,
    pub address: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct GetAccounts {
    pub subaddress_accounts: Vec<SubAddressAccount>,
    pub total_balance: u64,
    pub total_unlocked_balance: u64,
}

#[derive(Deserialize, Debug, Clone)]
pub struct SubAddressAccount {
    pub account_index: u32,
    pub balance: u32,
    pub base_address: String,
    pub label: String,
    pub tag: String,
    pub unlocked_balance: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Destination {
    pub amount: u64,
    pub address: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Transfer {
    pub amount: u64,
    pub fee: u64,
    pub multisig_txset: String,
    #[serde(default)]
    pub tx_blob: String,
    pub tx_hash: String,
    #[serde(default, deserialize_with = "opt_key_from_blank")]
    pub tx_key: Option<monero::PrivateKey>,
    #[serde(default)]
    pub tx_metadata: String,
    pub unsigned_txset: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct RelayTx {
    pub tx_hash: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct SubmitTransaction {
    pub tx_hash: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct BlockHeight {
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(from = "CheckTxKeyResponse")]
pub struct CheckTxKey {
    pub confirmations: u64,
    pub received: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct GetTransferByTxid {
    pub transfer: TransferInfo,
}

/// Response of `get_transfers`: the wallet's recorded transfers. Only the
/// outgoing and pending lists are requested (the fields we reconcile lock
/// attempts against); the other categories deserialize as absent. Daemon
/// versions differ on the wire format: recent daemons send each category as
/// a SEQUENCE of transfer entries (txid inside the entry), others as a map
/// keyed by txid - both are accepted.
#[derive(Clone, Debug, Deserialize)]
pub struct GetTransfers {
    #[serde(default, deserialize_with = "transfers_sequence_or_map")]
    pub out: HashMap<String, TransferInfo>,
    #[serde(default, deserialize_with = "transfers_sequence_or_map")]
    pub pending: HashMap<String, TransferInfo>,
}

fn transfers_sequence_or_map<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, TransferInfo>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;

    match value {
        serde_json::Value::Array(items) => {
            let mut result = HashMap::new();
            for (index, item) in items.into_iter().enumerate() {
                let info: TransferInfo =
                    serde_json::from_value(item).map_err(serde::de::Error::custom)?;
                let key = if info.txid.is_empty() {
                    index.to_string()
                } else {
                    info.txid.clone()
                };
                result.insert(key, info);
            }
            Ok(result)
        }
        serde_json::Value::Object(map) => {
            let mut result = HashMap::new();
            for (key, item) in map {
                let info: TransferInfo =
                    serde_json::from_value(item).map_err(serde::de::Error::custom)?;
                result.insert(key, info);
            }
            Ok(result)
        }
        _ => Err(serde::de::Error::custom(
            "expected a sequence or map of transfers",
        )),
    }
}

/// The tx key the wallet recorded for one of its own outgoing transfers.
#[derive(Clone, Debug, Deserialize)]
pub struct GetTxKey {
    pub tx_key: String,
}

/// An entry of the wallet's transfer history. All fields beyond
/// `double_spend_seen` default on missing: depending on the direction and
/// RPC method, not all of them are present.
#[derive(Clone, Debug, Deserialize)]
pub struct TransferInfo {
    #[serde(default)]
    pub double_spend_seen: bool,
    #[serde(default)]
    pub txid: String,
    #[serde(default)]
    pub amount: u64,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub destinations: Vec<Destination>,
    #[serde(default)]
    pub confirmations: u64,
    #[serde(default, deserialize_with = "opt_key_from_blank")]
    pub tx_key: Option<monero::PrivateKey>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
struct CheckTxKeyResponse {
    pub confirmations: u64,
    pub received: u64,
}

impl From<CheckTxKeyResponse> for CheckTxKey {
    fn from(response: CheckTxKeyResponse) -> Self {
        // Due to a bug in monerod that causes check_tx_key confirmations
        // to overflow we safeguard the confirmations to avoid unwanted
        // side effects.
        let confirmations = if response.confirmations > u64::MAX - 1000 {
            0
        } else {
            response.confirmations
        };

        CheckTxKey {
            confirmations,
            received: response.received,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct GenerateFromKeys {
    pub address: String,
    pub info: String,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Refreshed {
    pub blocks_fetched: u32,
    pub received_money: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SweepAll {
    #[serde(default)]
    pub tx_hash_list: Vec<String>,
    #[serde(default)]
    pub tx_key_list: Vec<String>,
    #[serde(default)]
    pub tx_blob_list: Vec<String>,
    #[serde(default)]
    pub tx_metadata_list: Vec<String>,
    #[serde(default)]
    pub fee_list: Vec<u64>,
}

#[derive(Debug, Copy, Clone, Deserialize)]
pub struct Version {
    pub version: u32,
}

pub type WalletCreated = Empty;
pub type WalletClosed = Empty;
pub type WalletOpened = Empty;

/// Zero-sized struct to allow serde to deserialize an empty JSON object.
///
/// With `serde`, an empty JSON object (`{ }`) does not deserialize into Rust's
/// `()`. With the adoption of `jsonrpc_client`, we need to be explicit about
/// what the response of every RPC call is. Unfortunately, monerod likes to
/// return empty objects instead of `null`s in certain cases. We use this struct
/// to all the "deserialization" to happily continue.
#[derive(Debug, Copy, Clone, Deserialize)]
pub struct Empty {}

fn opt_key_from_blank<'de, D>(deserializer: D) -> Result<Option<monero::PrivateKey>, D::Error>
where
    D: Deserializer<'de>,
{
    let string = String::deserialize(deserializer)?;

    if string.is_empty() {
        return Ok(None);
    }

    Ok(Some(string.parse().map_err(D::Error::custom)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonrpc_client::Response;

    #[test]
    fn parses_monero_digest_challenge() {
        let challenge = parse_digest_challenge(
            r#"Digest qop="auth",algorithm=MD5,realm="monero-rpc",nonce="NP2UW51EYN8wH2RrpVqH7A==",stale=false"#,
        )
        .expect("valid digest challenge");

        assert_eq!(challenge.realm, "monero-rpc");
        assert_eq!(challenge.nonce, "NP2UW51EYN8wH2RrpVqH7A==");
        assert_eq!(challenge.algorithm, "MD5");
        assert_eq!(challenge.qop.as_deref(), Some("auth"));
    }

    #[test]
    fn builds_digest_authorization() {
        let client = DigestClient {
            inner: reqwest::Client::new(),
            username: "user".to_owned(),
            password: "pass".to_owned(),
            nonce_count: Arc::new(AtomicU32::new(0)),
        };
        let challenge = DigestChallenge {
            realm: "monero-rpc".to_owned(),
            nonce: "nonce".to_owned(),
            opaque: None,
            algorithm: "MD5".to_owned(),
            qop: Some("auth".to_owned()),
        };

        let authorization = client
            .authorization(&challenge, "POST", "/json_rpc")
            .expect("digest authorization");

        assert!(authorization.starts_with("Digest username=\"user\""));
        assert!(authorization.contains("realm=\"monero-rpc\""));
        assert!(authorization.contains("qop=auth"));
        assert!(authorization.contains("algorithm=MD5"));
    }

    #[test]
    fn can_deserialize_sweep_all_response() {
        let response = r#"{
          "id": "0",
          "jsonrpc": "2.0",
          "result": {
            "amount_list": [29921410000],
            "fee_list": [78590000],
            "multisig_txset": "",
            "tx_hash_list": ["c1d8cfa87d445c1915a59d67be3e93ba8a29018640cf69b465f07b1840a8f8c8"],
            "unsigned_txset": "",
            "weight_list": [1448]
          }
        }"#;

        let _: Response<SweepAll> = serde_json::from_str(response).unwrap();
    }

    #[test]
    fn can_deserialize_create_wallet() {
        let response = r#"{
          "id": 0,
          "jsonrpc": "2.0",
          "result": {
          }
        }"#;

        let _: Response<WalletCreated> = serde_json::from_str(response).unwrap();
    }
    /// Reconciliation of lock attempts reads the wallet's outgoing/pending
    /// history. Recent daemons (observed live) return each category as a
    /// SEQUENCE of entries; entries carry txid/amount/address but NOT the tx
    /// key (that requires the dedicated get_tx_key RPC).
    #[test]
    fn can_deserialize_get_transfers_sequence_format() {
        let response = r#"{
          "id": "0",
          "jsonrpc": "2.0",
          "result": {
            "out": [
              {
                "address": "53H3QthYLckeCXh9u38vohb2gZ4QgEG3FMWHNxccR6MqV1LdDVYwF1FKsRJPj4tTupWLf9JtGPBcn2MVN6c9oR7p5Uf7JdJ",
                "amount": 10000,
                "confirmations": 11,
                "fee": 300000000,
                "destinations": [{ "address": "57euRLeG", "amount": 10000 }],
                "subaddr_indices": [{ "major": 0, "minor": 0 }],
                "timestamp": 1791997738,
                "txid": "abc123",
                "type": "out"
              }
            ],
            "pending": [
              {
                "address": "53H3QthYLckeCXh9u38vohb2gZ4QgEG3FMWHNxccR6MqV1LdDVYwF1FKsRJPj4tTupWLf9JtGPBcn2MVN6c9oR7p5Uf7JdJ",
                "amount": 20000,
                "txid": "def456",
                "type": "pending"
              }
            ]
          }
        }"#;

        let response: Response<GetTransfers> = serde_json::from_str(response).unwrap();
        let transfers =
            Result::<GetTransfers, jsonrpc_client::JsonRpcError>::from(response.payload)
                .expect("a successful get_transfers response carries a result");
        assert_eq!(transfers.out.len(), 1);
        assert_eq!(transfers.out["abc123"].amount, 10_000);
        assert_eq!(transfers.out["abc123"].txid, "abc123");
        assert_eq!(
            transfers.out["abc123"].destinations[0].address, "57euRLeG",
            "outgoing recipient must deserialize from destinations"
        );

        assert_eq!(transfers.pending.len(), 1);
        assert_eq!(transfers.pending["def456"].amount, 20_000);
    }

    /// Other daemon versions key the categories as maps of txid → entry;
    /// both shapes must deserialize.
    #[test]
    fn can_deserialize_get_transfers_map_format() {
        let response = r#"{
          "id": "0",
          "jsonrpc": "2.0",
          "result": {
            "out": {
              "abc123": {
                "address": "53H3QthYLckeCXh9u38vohb2gZ4QgEG3FMWHNxccR6MqV1LdDVYwF1FKsRJPj4tTupWLf9JtGPBcn2MVN6c9oR7p5Uf7JdJ",
                "amount": 10000,
                "txid": "abc123",
                "tx_key": "0100000000000000000000000000000000000000000000000000000000000000",
                "type": "out"
              }
            },
            "pending": {
              "def456": {
                "address": "53H3QthYLckeCXh9u38vohb2gZ4QgEG3FMWHNxccR6MqV1LdDVYwF1FKsRJPj4tTupWLf9JtGPBcn2MVN6c9oR7p5Uf7JdJ",
                "amount": 20000,
                "txid": "def456",
                "tx_key": "",
                "type": "pending"
              }
            }
          }
        }"#;

        let response: Response<GetTransfers> = serde_json::from_str(response).unwrap();
        let transfers =
            Result::<GetTransfers, jsonrpc_client::JsonRpcError>::from(response.payload)
                .expect("a successful get_transfers response carries a result");
        assert_eq!(transfers.out["abc123"].amount, 10_000);
        assert!(
            transfers.out["abc123"].tx_key.is_some(),
            "a recorded tx key deserializes when present"
        );
        assert_eq!(
            transfers.pending["def456"].tx_key, None,
            "a blank tx_key deserializes as None"
        );
    }

    /// The reconciliation flow fetches the recorded tx key through the
    /// dedicated get_tx_key RPC.
    #[test]
    fn can_deserialize_get_tx_key() {
        let response = r#"{
          "id": "0",
          "jsonrpc": "2.0",
          "result": {
            "tx_key": "6b2ef8f1e162f063bde3a1e2df2cb14ff2ef8b6779fb3e1ed5241f7d2b532b0a"
          }
        }"#;

        let response: Response<GetTxKey> = serde_json::from_str(response).unwrap();
        let key = Result::<GetTxKey, jsonrpc_client::JsonRpcError>::from(response.payload)
            .expect("a successful get_tx_key response carries a result");
        assert_eq!(key.tx_key.len(), 64);
    }

    /// Incoming-only wallets (and other absent categories) must deserialize:
    /// `out`/`pending` default to empty when the daemon omits them.
    #[test]
    fn get_transfers_defaults_missing_categories_to_empty() {
        let response = r#"{
          "id": "0",
          "jsonrpc": "2.0",
          "result": {
            "in": {
              "in-tx": { "amount": 5 }
            }
          }
        }"#;

        let response: Response<GetTransfers> = serde_json::from_str(response).unwrap();
        let transfers =
            Result::<GetTransfers, jsonrpc_client::JsonRpcError>::from(response.payload)
                .expect("a successful get_transfers response carries a result");
        assert_eq!(transfers.out.len(), 0);
        assert_eq!(transfers.pending.len(), 0);
    }
    #[test]
    fn transfer_defaults_optional_construct_fields() {
        let response = r#"{
          "amount": 10000,
          "fee": 300,
          "multisig_txset": "",
          "tx_hash": "abc123",
          "unsigned_txset": ""
        }"#;

        let transfer: Transfer = serde_json::from_str(response).unwrap();
        assert_eq!(transfer.tx_hash, "abc123");
        assert!(transfer.tx_blob.is_empty());
        assert!(transfer.tx_metadata.is_empty());
        assert!(transfer.tx_key.is_none());
    }

    #[test]
    fn sweep_all_deserializes_constructed_fields() {
        let response = r#"{
          "tx_hash_list": ["abc123"],
          "tx_key_list": ["key"],
          "tx_blob_list": ["blob"],
          "tx_metadata_list": ["metadata"],
          "fee_list": [300]
        }"#;

        let sweep: SweepAll = serde_json::from_str(response).unwrap();
        assert_eq!(sweep.tx_hash_list, vec!["abc123"]);
        assert_eq!(sweep.tx_metadata_list, vec!["metadata"]);
        assert_eq!(sweep.fee_list, vec![300]);
    }

    #[test]
    fn relay_and_submit_responses_deserialize() {
        let relay: RelayTx = serde_json::from_str(r#"{ "tx_hash": "abc123" }"#).unwrap();
        assert_eq!(relay.tx_hash, "abc123");

        let submit: SubmitTransaction =
            serde_json::from_str(r#"{ "tx_hash": "def456" }"#).unwrap();
        assert_eq!(submit.tx_hash, "def456");
    }

}
