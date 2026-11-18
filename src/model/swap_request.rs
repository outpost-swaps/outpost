use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SwapRequest {
    pub uuid: String,
    pub seed_base64: String,
    pub xmr_receive_address: String,
    pub electrum_url: String,
    pub proxy: String,
    pub libp2p_peer_address: String,
    pub xmr_rpc_endpoint: String,
    pub testnet: bool,
    pub proxy_port: u16,
    pub refund_address: String,
    pub swaps_account: u64,
    #[serde(default)]
    pub swaps_receive_index: u32,
    #[serde(default)]
    pub swaps_change_index: u32,
    #[serde(default)]
    pub dojo_url: Option<String>,
    #[serde(default)]
    pub dojo_api_key: Option<String>,
    #[serde(default)]
    pub xmr_daemon_url: Option<String>,
    /// Extra Electrum endpoints the wallet may fail over to, besides
    /// `electrum_url`. Absent in requests from older GUIs.
    #[serde(default)]
    pub electrum_pool_urls: Vec<String>,
    /// Extra Monero nodes that must agree Alice's lock exists before the redeem signature is
    /// released (see `swap::monero::witness`). Empty when the user's own node is trusted alone.
    #[serde(default)]
    pub xmr_witness_urls: Vec<String>,
    /// The policy needs witnesses for this swap. If it is set and none could be configured,
    /// the redeem signature is never released.
    #[serde(default)]
    pub xmr_witness_required: bool,
    /// How many witnesses are used. When it is more than zero, `xmr_witness_urls` is a list of
    /// candidates in order of preference: each is checked before the swap starts, and the first
    /// `xmr_witness_count` that work are used. Zero (an older GUI) means every URL is used as given.
    #[serde(default)]
    pub xmr_witness_count: usize,
    /// The seller fee rate (sat/kvB) the user accepted for this one swap after a "fees too high"
    /// refusal, as the seller asked it: it raises the upper limit (plus a margin) and reserves the
    /// lock fee that rate forces (`seller_fee`, matrix S-01). Absent normally.
    #[serde(default)]
    pub accepted_seller_fee_rate_sat_kvb: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dojo_fee_credentials_from_the_gui_request() {
        let json = r#"{
            "uuid":"swap-id",
            "seedBase64":"seed",
            "xmrReceiveAddress":"xmr",
            "electrumUrl":"tcp://electrum.onion:50001",
            "proxy":"127.0.0.1:9050",
            "libp2pPeerAddress":"peer",
            "xmrRpcEndpoint":"http://127.0.0.1:38083/json_rpc",
            "testnet":true,
            "proxyPort":9050,
            "refundAddress":"refund",
            "swapsAccount":2147483643,
            "swapsReceiveIndex":46,
            "swapsChangeIndex":6,
            "dojoUrl":"http://dojo.onion/test/v2",
            "dojoApiKey":"dojo-key"
        }"#;

        let request: SwapRequest = serde_json::from_str(json).unwrap();

        assert_eq!(
            request.dojo_url.as_deref(),
            Some("http://dojo.onion/test/v2")
        );
        assert_eq!(request.dojo_api_key.as_deref(), Some("dojo-key"));
        // Older GUIs do not send a pool; the request must still parse.
        assert!(request.electrum_pool_urls.is_empty());
    }

    #[test]
    fn parses_the_electrum_pool_from_the_gui_request() {
        let json = r#"{
            "uuid":"swap-id",
            "seedBase64":"seed",
            "xmrReceiveAddress":"xmr",
            "electrumUrl":"tcp://a.onion:50001",
            "proxy":"127.0.0.1:9050",
            "libp2pPeerAddress":"peer",
            "xmrRpcEndpoint":"http://127.0.0.1:38083/json_rpc",
            "testnet":true,
            "proxyPort":9050,
            "refundAddress":"refund",
            "swapsAccount":2147483643,
            "electrumPoolUrls":["tcp://a.onion:50001","tcp://b.onion:50001"]
        }"#;

        let request: SwapRequest = serde_json::from_str(json).unwrap();

        assert_eq!(
            request.electrum_pool_urls,
            vec!["tcp://a.onion:50001", "tcp://b.onion:50001"]
        );
    }

    #[test]
    fn witness_nodes_are_read_from_the_request_and_default_to_none() {
        let with = r#"{"uuid":"swap-id","seedBase64":"seed","xmrReceiveAddress":"xmr","electrumUrl":"tcp://e.onion:50001","proxy":"127.0.0.1:9050","libp2pPeerAddress":"peer","xmrRpcEndpoint":"http://127.0.0.1:1","testnet":false,"proxyPort":9050,"refundAddress":"bc1q","swapsAccount":0,"xmrWitnessUrls":["http://a.onion:18081","https://b.example.org:18081"],"xmrWitnessRequired":true}"#;
        let parsed: SwapRequest = serde_json::from_str(with).unwrap();
        assert_eq!(parsed.xmr_witness_urls, vec!["http://a.onion:18081", "https://b.example.org:18081"]);
        assert!(parsed.xmr_witness_required);

        // requests from a GUI that does not know about witnesses
        let without = r#"{"uuid":"swap-id","seedBase64":"seed","xmrReceiveAddress":"xmr","electrumUrl":"tcp://e.onion:50001","proxy":"127.0.0.1:9050","libp2pPeerAddress":"peer","xmrRpcEndpoint":"http://127.0.0.1:1","testnet":false,"proxyPort":9050,"refundAddress":"bc1q","swapsAccount":0}"#;
        let parsed: SwapRequest = serde_json::from_str(without).unwrap();
        assert!(parsed.xmr_witness_urls.is_empty());
        assert!(!parsed.xmr_witness_required);
        assert_eq!(parsed.xmr_witness_count, 0);

        let counted = with.replace(r#""xmrWitnessRequired":true"#, r#""xmrWitnessRequired":true,"xmrWitnessCount":2"#);
        let parsed: SwapRequest = serde_json::from_str(&counted).unwrap();
        assert_eq!(parsed.xmr_witness_count, 2);
    }
}

#[cfg(test)]
mod accepted_seller_fee_tests {
    use super::*;

    fn request(extra: &str) -> SwapRequest {
        serde_json::from_str(&format!(r#"{{"uuid":"id","seedBase64":"s","xmrReceiveAddress":"x","electrumUrl":"tcp://e:1","proxy":"p","libp2pPeerAddress":"a","xmrRpcEndpoint":"r","testnet":true,"proxyPort":9050,"refundAddress":"tb1q","swapsAccount":1{extra}}}"#)).unwrap()
    }

    #[test]
    fn the_accepted_rate_is_read_and_absent_by_default() {
        assert_eq!(request("").accepted_seller_fee_rate_sat_kvb, None);
        assert_eq!(request(r#","acceptedSellerFeeRateSatKvb":6746"#).accepted_seller_fee_rate_sat_kvb, Some(6746));
    }
}

