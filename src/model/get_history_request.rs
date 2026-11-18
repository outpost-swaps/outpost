use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GetHistoryRequest {
    pub testnet: bool,
    #[serde(default)]
    pub seed_base64: Option<String>,
}