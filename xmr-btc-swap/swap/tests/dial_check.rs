// temporary manual probe: tor socks dial to a maker onion
#[tokio::test]
#[ignore = "manual probe: requires Tor SOCKS on 9155"]
async fn tor_socks_dial_maker_onion() {
    use tokio_socks::tcp::Socks5Stream;
    use std::net::Ipv4Addr;
    let stream = Socks5Stream::connect(
        (Ipv4Addr::LOCALHOST, 9155),
        "u7akjfnrz4bjlvole5erjiok6wysthz6xgntxglvx2m7br3663wvyuqd.onion:9939".to_string(),
    )
    .await;
    match stream {
        Ok(_) => println!("MAKER ONION DIAL: CONNECTED"),
        Err(e) => println!("MAKER ONION DIAL: FAILED {:?}", e),
    }
}
