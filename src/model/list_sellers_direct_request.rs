use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListSellersDirectRequest {
    pub seed_base64: String,
    pub maker_addresses: Vec<String>,
    pub proxy_port: u16,
    pub testnet: bool,
}
