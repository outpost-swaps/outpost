use anyhow::{anyhow, bail, Context};

const LATEST_RELEASE_URL: &str = "https://github.com/comit-network/xmr-btc-swap/releases/latest";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Version {
    Current,
    Available,
}

/// Check the latest release from GitHub API through Tor.
pub async fn check_latest_version(
    current_version: &str,
    tor_socks5_port: u16,
) -> anyhow::Result<Version> {
    if tor_socks5_port == 0 {
        bail!("Tor is required: refusing to check for updates without a SOCKS5 proxy");
    }
    let proxy = reqwest::Proxy::all(format!("socks5h://127.0.0.1:{tor_socks5_port}"))
        .context("Failed to construct Tor proxy URL")?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        // This is a best-effort GitHub lookup through Tor. A stalled circuit
        // must never block ASB or CLI startup before wallet initialization.
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("Failed to build update-check client")?;
    let response = client.get(LATEST_RELEASE_URL).send().await?;
    let e = "Failed to get latest release.";
    let download_url = response.url();
    let segments = download_url.path_segments().ok_or_else(|| anyhow!(e))?;
    let latest_version = segments.last().ok_or_else(|| anyhow!(e))?;

    let result = if is_latest_version(current_version, latest_version) {
        Version::Current
    } else {
        tracing::warn!(%current_version, %latest_version, %download_url,
            "You are not on the latest version",
        );
        Version::Available
    };

    Ok(result)
}

// todo: naive implementation can be improved using semver
fn is_latest_version(current: &str, latest: &str) -> bool {
    current == latest
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn it_compares_versions() {
        assert!(is_latest_version("0.10.2", "0.10.2"));
        assert!(!is_latest_version("0.10.2", "0.10.3"));
        assert!(!is_latest_version("0.10.2", "0.11.0"));
    }

    #[tokio::test]
    async fn refuses_to_check_for_updates_without_tor() {
        assert!(check_latest_version("0.11.0", 0).await.is_err());
    }

    #[tokio::test]
    #[ignore = "For local testing, makes http requests to github."]
    async fn it_compares_with_github() {
        let result = check_latest_version("0.11.0", 9050).await.unwrap();
        assert_eq!(result, Version::Available);

        let result = check_latest_version("0.11.1", 9050).await.unwrap();
        assert_eq!(result, Version::Current);
    }
}
