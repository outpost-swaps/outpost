//! One rule for every Rust HTTP client: a destination that is not on this
//! machine is only reached through Tor. A missing Tor port is an error, never
//! "connect directly".

use anyhow::{bail, Result};
use url::Url;

/// Whether `url` points at this machine (a local service or a test server).
pub fn url_is_local(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => {
            // Non-special schemes such as `tcp://` keep an IP literal as a
            // "domain" string, so parse it (brackets stripped for IPv6).
            let host = domain.trim_start_matches('[').trim_end_matches(']');
            if host.eq_ignore_ascii_case("localhost") {
                return true;
            }
            host.parse::<std::net::IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false)
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// The SOCKS5h proxy URL to use for `destination`.
///
/// - local destination: `None` (no proxy needed, nothing leaves the machine)
/// - otherwise with a Tor port: `socks5h://127.0.0.1:<port>` (DNS resolved by Tor)
/// - otherwise (port 0): an error, so the caller cannot go out directly
pub fn socks_proxy_for(tor_socks5_port: u16, destination: &Url) -> Result<Option<String>> {
    if url_is_local(destination) {
        return Ok(None);
    }
    if tor_socks5_port == 0 {
        bail!(
            "refusing to connect to {} without Tor: no Tor SOCKS port is configured",
            destination.host_str().unwrap_or("a remote host")
        );
    }
    Ok(Some(format!("socks5h://127.0.0.1:{tor_socks5_port}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn a_remote_destination_without_a_tor_port_is_refused() {
        assert!(socks_proxy_for(0, &url("https://dojo.example/v2")).is_err());
        assert!(socks_proxy_for(0, &url("http://abcdefghijklmnop.onion/test/v2")).is_err());
    }

    #[test]
    fn a_remote_destination_uses_socks5h_through_the_tor_port() {
        assert_eq!(
            socks_proxy_for(9050, &url("https://dojo.example/v2")).unwrap().as_deref(),
            Some("socks5h://127.0.0.1:9050")
        );
    }

    #[test]
    fn a_local_destination_needs_no_proxy_even_without_a_tor_port() {
        assert_eq!(socks_proxy_for(0, &url("http://127.0.0.1:8080/")).unwrap(), None);
        assert_eq!(socks_proxy_for(0, &url("http://localhost:8080/")).unwrap(), None);
        assert_eq!(socks_proxy_for(0, &url("http://[::1]:8080/")).unwrap(), None);
    }

    #[test]
    fn a_non_special_scheme_loopback_literal_is_still_local() {
        assert!(url_is_local(&url("tcp://127.0.0.1:50001")));
        assert!(url_is_local(&url("tcp://localhost:50001")));
        assert!(url_is_local(&url("tcp://[::1]:50001")));
        assert!(!url_is_local(&url("tcp://abcdefghijklmnop.onion:50001")));
    }

    #[test]
    fn lookalike_hosts_are_not_local() {
        assert!(!url_is_local(&url("http://127.0.0.1.evil.example/")));
        assert!(!url_is_local(&url("http://localhost.evil.example/")));
    }
}
