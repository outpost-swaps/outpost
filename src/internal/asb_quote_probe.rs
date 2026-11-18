// temporary live probe: direct quote from OUR testnet ASB (no rendezvous)
// cargo test --lib -- --ignored --nocapture asb_quote_probe
#[cfg(test)]
mod asb_quote_probe {
    use crate::internal::internal_list_sellers::{internal_list_sellers_direct};
    use swap::seed::Seed;
    use libp2p::Multiaddr;

    #[tokio::test]
    #[ignore = "live probe: requires our testnet ASB running + Tor SOCKS on 9155"]
    async fn direct_quote_from_our_asb() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
        let seed = Seed::from([9u8; 64]);
        let identity = seed.derive_libp2p_identity();
        let addr: Multiaddr = "/onion3/ubiamiotqmokfiidjingnx3cdlzyeo2tq4kbqoba5k6oqotknstfrgyd:9939/p2p/12D3KooWEMq58eksT5eJPookK5SGVkXaMSYuNMHdorKRoWYLyui8"
            .parse()
            .unwrap();
        let sellers = internal_list_sellers_direct(vec![addr], 9155, identity)
            .await
            .expect("direct probe failed");
        for seller in &sellers {
            println!("PROBE RESULT: {:?}", seller);
        }
        assert!(!sellers.is_empty(), "no quote from our own ASB");
    }
}
