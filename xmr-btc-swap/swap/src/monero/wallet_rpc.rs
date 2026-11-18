use crate::util;
use ::monero::Network;
use anyhow::{bail, Context, Error, Result};
use big_bytes::BigByte;
use futures::{StreamExt, TryStreamExt};
use jni::JNIEnv;
use monero_rpc::wallet::{Client, MoneroWalletRpc as _};
use reqwest::header::CONTENT_LENGTH;
use reqwest::{Proxy, Url};
use serde::Deserialize;
use std::fmt;
use std::fmt::{Debug, Display, Formatter};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::fs::{remove_file, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio_util::codec::{BytesCodec, FramedRead};
use tokio_util::io::StreamReader;
use uuid::Uuid;

// See: https://monero.fail (the 2026-10-11 probe pass pruned nodes that are
// NXDOMAIN/refused or failed two consecutive passes; see notes/endpoint-inventory.md)
// We don't need any testnet nodes because we don't support testnet at all
const MONERO_DAEMONS: [MoneroDaemon; 9] = [
    MoneroDaemon::new("xmr-node.cakewallet.com", 18081, Network::Mainnet),
    MoneroDaemon::new("nodex.monerujo.io", 18081, Network::Mainnet),
    MoneroDaemon::new("nodes.hashvault.pro", 18081, Network::Mainnet),
    MoneroDaemon::new("p2pmd.xmrvsbeast.com", 18081, Network::Mainnet),
    MoneroDaemon::new("node.monerodevs.org", 18089, Network::Mainnet),
    MoneroDaemon::new("xmr.litepay.ch", 18081, Network::Mainnet),
    MoneroDaemon::new("stagenet.xmr-tw.org", 38081, Network::Stagenet),
    MoneroDaemon::new("node.monerodevs.org", 38089, Network::Stagenet),
    MoneroDaemon::new("xmr-lux.boldsuck.org", 38081, Network::Stagenet),
];

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
compile_error!("unsupported operating system");

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const DOWNLOAD_URL: &str = "https://downloads.getmonero.org/cli/monero-mac-x64-v0.18.5.4.tar.bz2";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const DOWNLOAD_URL: &str = "https://downloads.getmonero.org/cli/monero-mac-armv8-v0.18.5.4.tar.bz2";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const DOWNLOAD_URL: &str = "https://downloads.getmonero.org/cli/monero-linux-x64-v0.18.5.4.tar.bz2";

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const DOWNLOAD_URL: &str =
    "https://downloads.getmonero.org/cli/monero-linux-armv8-v0.18.5.4.tar.bz2";

#[cfg(target_os = "windows")]
const DOWNLOAD_URL: &str = "https://downloads.getmonero.org/cli/monero-win-x64-v0.18.5.4.zip";

// SHA-256 of the compressed archive, from the GPG-signed hash list at
// https://www.getmonero.org/downloads/hashes.txt (signed by binaryFate).
// Re-verify the signature against Monero's GPG key when bumping versions.
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const EXPECTED_ARCHIVE_SHA256: &str =
    "9fefa7f5f7ae60fbd8d0ad18842b76c56ed161eca4ffb4f8b5f4800e5033b146";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXPECTED_ARCHIVE_SHA256: &str =
    "d457de7c8a1ad42e3232fdea6d9f9003be9652143fb8f719c1fc906aa6b4917b";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const EXPECTED_ARCHIVE_SHA256: &str =
    "ba2928962c101b609434bf8c1b43ad59ebe96c5525977284ca599d8bf0f02d10";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const EXPECTED_ARCHIVE_SHA256: &str =
    "d20d71e6522b41eb8e549c3425f7f4622c9c9a448fc7147525393fd0a6b59703";
#[cfg(target_os = "windows")]
const EXPECTED_ARCHIVE_SHA256: &str =
    "1ccac3b476ba75bf6dfdfa848f6520a79b64cdf3457a160eaa0dad82d5a2a042";

#[cfg(any(target_os = "macos", target_os = "linux"))]
const PACKED_FILE: &str = "monero-wallet-rpc";

#[cfg(target_os = "windows")]
const PACKED_FILE: &str = "monero-wallet-rpc.exe";

const WALLET_RPC_VERSION: &str = "v0.18.5.4";
const EXEC_CHECKSUM_FILE: &str = "monero-wallet-rpc.sha256";

// A malicious or broken server must not be able to exhaust disk or memory
// before the pinned archive hash is checked. The official CLI archives are
// well below these limits; the wallet-RPC executable itself is ~30 MiB.
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_UNPACKED_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_WALLET_RPC_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("monero wallet rpc executable not found in downloaded archive")]
pub struct ExecutableNotFoundInArchive;

pub struct WalletRpcProcess {
    _child: Child,
    port: u16,
    rpc_user: String,
    rpc_password: String,
}

struct MoneroDaemon {
    address: &'static str,
    port: u16,
    network: Network,
}

impl MoneroDaemon {
    const fn new(address: &'static str, port: u16, network: Network) -> Self {
        Self {
            address,
            port,
            network,
        }
    }

    /// Checks if the Monero daemon is available by sending a request to its `get_info` endpoint.
    async fn is_available(&self, client: &reqwest::Client) -> Result<bool, Error> {
        let url = format!("http://{}:{}/get_info", self.address, self.port);
        let res = client
            .get(url)
            .send()
            .await
            .context("Failed to send request to get_info endpoint")?;

        let json: MoneroDaemonGetInfoResponse = res
            .json()
            .await
            .context("Failed to deserialize daemon get_info response")?;

        let is_status_ok = json.status == "OK";
        let is_synchronized = json.synchronized;
        let is_correct_network = match self.network {
            Network::Mainnet => json.mainnet,
            Network::Stagenet => json.stagenet,
            Network::Testnet => json.testnet,
        };

        Ok(is_status_ok && is_synchronized && is_correct_network)
    }
}

impl Display for MoneroDaemon {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.address, self.port)
    }
}

#[derive(Deserialize)]
struct MoneroDaemonGetInfoResponse {
    status: String,
    synchronized: bool,
    mainnet: bool,
    stagenet: bool,
    testnet: bool,
}

/// Chooses an available Monero daemon based on the specified network.
async fn choose_monero_daemon(network: Network) -> Result<&'static MoneroDaemon, Error> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .https_only(false)
        .build()?;

    // We only want to check for daemons that match the specified network
    let network_matching_daemons = MONERO_DAEMONS
        .iter()
        .filter(|daemon| daemon.network == network);

    for daemon in network_matching_daemons {
        match daemon.is_available(&client).await {
            Ok(true) => {
                tracing::debug!(%daemon, "Found available Monero daemon");
                return Ok(daemon);
            }
            Err(err) => {
                tracing::debug!(%err, %daemon, "Failed to connect to Monero daemon");
                continue;
            }
            Ok(false) => continue,
        }
    }

    bail!("No Monero daemon could be found. Please specify one manually or try again later.")
}

impl WalletRpcProcess {
    pub fn endpoint(&self) -> Url {
        Url::parse(&format!(
            "http://{}:{}@127.0.0.1:{}/json_rpc",
            self.rpc_user, self.rpc_password, self.port
        ))
        .expect("Static url template is always valid")
    }
}

/// A download attempt that cannot even connect within this long is given up (Tor circuit + TLS).
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(60);
/// A download that receives no bytes at all for this long is stalled and retried. A slow download
/// keeps going: only silence counts. (2026-11-11: with no limit, a dead circuit hung first launch.)
const DOWNLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Attempts in all, each on its own Tor circuit.
const DOWNLOAD_ATTEMPTS: u32 = 3;

/// The SOCKS proxy for one attempt. Tor isolates streams by SOCKS username (IsolateSOCKSAuth is on
/// by default), so a different username per attempt gives a fresh circuit instead of the slow one.
fn download_proxy_url(proxy_string: &str, attempt: u32) -> String {
    format!("socks5h://outpost-download-{}:x@{}", attempt, proxy_string)
}

/// The next item of a download stream, or an error when nothing arrives within `limit`.
async fn next_or_stall<S>(stream: &mut S, limit: Duration) -> Result<Option<S::Item>>
where
    S: futures::Stream + Unpin,
{
    tokio::time::timeout(limit, stream.next())
        .await
        .map_err(|_| anyhow::anyhow!("the download stalled: no data for {} s", limit.as_secs()))
}

pub struct WalletRpc {
    working_dir: PathBuf,
}

impl WalletRpc {
    pub async fn new(
        env: Option<&JNIEnv<'_>>,
        working_dir: impl AsRef<Path>,
        proxy_string: String,
    ) -> Result<WalletRpc> {
        if proxy_string.is_empty() {
            bail!("Tor is required: refusing to download monero-wallet-rpc without a SOCKS5 proxy");
        }
        let working_dir = working_dir.as_ref();

        if !working_dir.exists() {
            tokio::fs::create_dir(working_dir).await?;
        }

        let monero_wallet_rpc = WalletRpc {
            working_dir: working_dir.to_path_buf(),
        };

        if monero_wallet_rpc.archive_path().exists() {
            remove_file(monero_wallet_rpc.archive_path()).await?;
        }

        // Verify the binary before executing even `--version`: an existing
        // wallet RPC must match the checksum written after a verified extract.
        let exec_path = monero_wallet_rpc.exec_path();
        tracing::debug!("RPC exec path: {}", exec_path.display());

        if exec_path.exists() {
            if !monero_wallet_rpc.verify_exec_checksum().await? {
                tracing::warn!(
                    "Removing monero-wallet-rpc with a missing or invalid checksum; it will be re-downloaded"
                );
                remove_file(&exec_path).await?;
                let _ = remove_file(monero_wallet_rpc.checksum_path()).await;
            } else {
                let output = Command::new(&exec_path).arg("--version").output().await?;
                let version = String::from_utf8_lossy(&output.stdout);
                tracing::debug!("RPC version output: {}", version);

                if !version.contains(WALLET_RPC_VERSION) {
                    tracing::info!("Removing old version of monero-wallet-rpc");
                    tokio::fs::remove_file(&exec_path).await?;
                    let _ = remove_file(monero_wallet_rpc.checksum_path()).await;
                }
            }
        }

        // if monero-wallet-rpc doesn't exist then download it
        if !monero_wallet_rpc.exec_path().exists() {
            let mut attempt = 1;
            loop {
                match monero_wallet_rpc.download_archive(env, &proxy_string, attempt).await {
                    Ok(()) => break,
                    Err(error) => {
                        let _ = tokio::fs::remove_file(monero_wallet_rpc.archive_path()).await;
                        if attempt >= DOWNLOAD_ATTEMPTS {
                            return Err(error.context(format!(
                                "Could not download monero-wallet-rpc after {} attempts over Tor",
                                DOWNLOAD_ATTEMPTS
                            )));
                        }
                        tracing::warn!(
                            attempt,
                            error = %format!("{:#}", error),
                            "monero-wallet-rpc download failed; retrying on a new Tor circuit"
                        );
                        attempt += 1;
                    }
                }
            }

            tracing::debug!("Extracting archive");
            Self::extract_archive(&monero_wallet_rpc).await?;
            monero_wallet_rpc.write_exec_checksum().await?;
        }
        Ok(monero_wallet_rpc)
    }

    /// One download attempt of the pinned archive: streams it through Tor, hashing as it goes, and
    /// fails closed on a size limit, a stall or a hash mismatch. The caller retries.
    async fn download_archive(
        &self,
        env: Option<&JNIEnv<'_>>,
        proxy_string: &str,
        attempt: u32,
    ) -> Result<()> {
        let mut options = OpenOptions::new();
        let mut file = options
            .read(true)
            .write(true)
            .create_new(true)
            .open(self.archive_path())
            .await?;

        let response = reqwest::Client::builder()
            .proxy(Proxy::all(download_proxy_url(proxy_string, attempt))?)
            .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
            .build()?
            .get(DOWNLOAD_URL)
            .send()
            .await?
            .error_for_status()?;

        let content_length = response
            .headers()
            .get(CONTENT_LENGTH)
            .context("The download has no Content-Length")?
            .to_str()
            .context("Failed to convert content-length to string")?
            .parse::<u64>()?;
        if let Err(error) = ensure_within_limit(
            content_length,
            MAX_ARCHIVE_BYTES,
            "monero-wallet-rpc archive",
        ) {
            let _ = tokio::fs::remove_file(self.archive_path()).await;
            return Err(error);
        }

        tracing::info!(
            "Downloading monero-wallet-rpc ({}) from {} (attempt {} of {})",
            content_length.big_byte(2),
            DOWNLOAD_URL,
            attempt,
            DOWNLOAD_ATTEMPTS
        );

        // Hash the compressed archive as it streams in, then verify against the
        // pinned official hash before extraction: a tampered or truncated
        // download must never reach execution.
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        let byte_stream = response
            .bytes_stream()
            .map_err(|err| std::io::Error::new(ErrorKind::Other, err))
            .map_ok(|bytes| {
                hasher.update(&bytes);
                bytes
            });

        #[cfg(not(target_os = "windows"))]
        let mut stream = FramedRead::new(
            async_compression::tokio::bufread::BzDecoder::new(StreamReader::new(byte_stream)),
            BytesCodec::new(),
        )
        .map_ok(|bytes| bytes.freeze());

        #[cfg(target_os = "windows")]
        let mut stream = FramedRead::new(StreamReader::new(byte_stream), BytesCodec::new())
            .map_ok(|bytes| bytes.freeze());

        let (mut received, mut notified): (u64, u64) = (0, 0);
        while let Some(chunk) = next_or_stall(&mut stream, DOWNLOAD_STALL_TIMEOUT).await? {
            let bytes = chunk?;
            received += bytes.len() as u64;
            if let Err(error) = ensure_within_limit(
                received,
                MAX_UNPACKED_ARCHIVE_BYTES,
                "decompressed monero-wallet-rpc archive",
            ) {
                drop(stream);
                let _ = tokio::fs::remove_file(self.archive_path()).await;
                return Err(error);
            }
            // the stream is decompressed as it is downloaded
            // file is compressed approx 3:1 in bz format. A malicious
            // server can send a zero Content-Length, so never divide by 0.
            let total = download_progress_total(content_length);
            let percent = 100 * received / total;
            if percent != notified {
                tracing::debug!("{}%", percent);
                notified = percent;
                if env.is_some() {
                    util::on_xmr_rpc_download_progress(env.unwrap(), percent);
                }
            }
            file.write_all(&bytes).await?;
        }

        file.flush().await?;

        drop(stream);
        let downloaded_hash: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{:02x}", byte))
            .collect();
        if downloaded_hash != EXPECTED_ARCHIVE_SHA256 {
            // Fail closed: remove the failed archive so a later run re-downloads.
            let _ = tokio::fs::remove_file(self.archive_path()).await;
            bail!(
                "Downloaded monero-wallet-rpc archive failed SHA-256 verification (got {}, expected {} from the GPG-signed Monero hash list) — refusing to execute it",
                downloaded_hash,
                EXPECTED_ARCHIVE_SHA256
            );
        }
        Ok(())
    }

    pub async fn run(
        &self,
        network: Network,
        daemon_address: Option<String>,
        proxy: Option<String>,
    ) -> Result<WalletRpcProcess> {
        let port = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await?
            .local_addr()?
            .port();

        let daemon_address = match daemon_address {
            Some(daemon_address) => daemon_address,
            None => choose_monero_daemon(network).await?.to_string(),
        };

        tracing::debug!(
            %daemon_address,
            %port,
            "Starting monero-wallet-rpc"
        );

        let network_flag = match network {
            Network::Mainnet => {
                vec![]
            }
            Network::Stagenet => {
                vec!["--stagenet"]
            }
            Network::Testnet => {
                vec!["--testnet"]
            }
        };

        let rpc_user = Uuid::new_v4().simple().to_string();
        let rpc_password = Uuid::new_v4().simple().to_string();

        let mut command = Command::new(self.exec_path());
        command
            .env("LANG", "en_AU.UTF-8")
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .args(network_flag)
            .arg("--daemon-address")
            .arg(daemon_address)
            .arg("--rpc-bind-port")
            .arg(format!("{}", port))
            .arg("--rpc-login")
            .arg(format!("{}:{}", rpc_user, rpc_password))
            .arg("--wallet-dir")
            .arg(self.working_dir.join("monero-data"));
        if let Some(proxy) = proxy {
            command
                .arg("--proxy")
                .arg(proxy)
                .arg("--daemon-ssl-allow-any-cert");
        }
        let mut child = command.spawn()?;

        let stdout = child
            .stdout
            .take()
            .expect("monero wallet rpc stdout was not piped parent process");

        let mut reader = BufReader::new(stdout).lines();

        #[cfg(not(target_os = "windows"))]
        while let Some(line) = reader.next_line().await? {
            if line.contains("Starting wallet RPC server") {
                break;
            }
        }

        // If we do not hear from the monero_wallet_rpc process for 3 seconds we assume
        // it is is ready
        #[cfg(target_os = "windows")]
        while let Ok(line) =
            tokio::time::timeout(std::time::Duration::from_secs(3), reader.next_line()).await
        {
            line?;
        }

        // Send an authenticated json rpc request to make sure
        // monero_wallet_rpc is ready.
        let endpoint = Url::parse(&format!(
            "http://{}:{}@127.0.0.1:{}/json_rpc",
            rpc_user, rpc_password, port
        ))?;
        Client::new(endpoint)?.get_version().await?;

        Ok(WalletRpcProcess {
            _child: child,
            port,
            rpc_user,
            rpc_password,
        })
    }

    fn archive_path(&self) -> PathBuf {
        self.working_dir.join("monero-cli-wallet.archive")
    }

    fn exec_path(&self) -> PathBuf {
        self.working_dir.join(PACKED_FILE)
    }

    fn checksum_path(&self) -> PathBuf {
        self.working_dir.join(EXEC_CHECKSUM_FILE)
    }

    async fn verify_exec_checksum(&self) -> Result<bool> {
        let expected = match tokio::fs::read_to_string(self.checksum_path()).await {
            Ok(expected) => expected,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };

        let actual = sha256_file(&self.exec_path()).await?;
        Ok(expected.trim() == actual)
    }

    async fn write_exec_checksum(&self) -> Result<()> {
        let checksum = sha256_file(&self.exec_path()).await?;
        tokio::fs::write(self.checksum_path(), format!("{}\n", checksum)).await?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    async fn extract_archive(monero_wallet_rpc: &Self) -> Result<()> {
        use tokio_tar::Archive;

        let mut options = OpenOptions::new();
        let file = options
            .read(true)
            .open(monero_wallet_rpc.archive_path())
            .await?;

        let mut ar = Archive::new(file);
        let mut entries = ar.entries()?;

        loop {
            match entries.next().await {
                Some(file) => {
                    let mut f = file?;
                    if f.path()?
                        .to_str()
                        .context("Could not find convert path to str in tar ball")?
                        .contains(PACKED_FILE)
                    {
                        let size = f
                            .header()
                            .size()
                            .context("Could not read the packed executable size")?;
                        ensure_within_limit(
                            size,
                            MAX_WALLET_RPC_BYTES,
                            "packed monero-wallet-rpc executable",
                        )?;
                        f.unpack(monero_wallet_rpc.exec_path()).await?;
                        break;
                    }
                }
                None => bail!(ExecutableNotFoundInArchive),
            }
        }

        remove_file(monero_wallet_rpc.archive_path()).await?;

        Ok(())
    }

    #[cfg(target_os = "windows")]
    async fn extract_archive(monero_wallet_rpc: &Self) -> Result<()> {
        use std::fs::File;
        use tokio::task::JoinHandle;
        use zip::ZipArchive;

        let archive_path = monero_wallet_rpc.archive_path();
        let exec_path = monero_wallet_rpc.exec_path();

        let extract: JoinHandle<Result<()>> = tokio::task::spawn_blocking(|| {
            let file = File::open(archive_path)?;
            let mut zip = ZipArchive::new(file)?;

            // zip 9 returns each name as a Result (names that cannot be decoded are skipped).
            let name = zip
                .file_names()
                .filter_map(|name| name.ok())
                .find(|name| name.contains(PACKED_FILE))
                .context(ExecutableNotFoundInArchive)?
                .to_string();

            let mut rpc = zip.by_name(&name)?;
            ensure_within_limit(
                rpc.size(),
                MAX_WALLET_RPC_BYTES,
                "packed monero-wallet-rpc executable",
            )?;
            let mut file = File::create(exec_path)?;
            std::io::copy(&mut rpc, &mut file)?;
            Ok(())
        });
        extract.await??;

        remove_file(monero_wallet_rpc.archive_path()).await?;

        Ok(())
    }
}

fn download_progress_total(content_length: u64) -> u64 {
    content_length.saturating_mul(3).max(1)
}

fn ensure_within_limit(value: u64, limit: u64, what: &str) -> Result<()> {
    if value > limit {
        bail!("{what} is too large ({value} bytes; limit {limit} bytes)");
    }
    Ok(())
}

async fn sha256_file(path: &Path) -> Result<String> {
    use sha2::Digest;

    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }

    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_content_length_does_not_panic_download_progress() {
        assert_eq!(download_progress_total(0), 1);
        assert_eq!(download_progress_total(10), 30);
    }

    #[test]
    fn archive_size_limits_reject_oversized_values() {
        assert!(ensure_within_limit(MAX_ARCHIVE_BYTES, MAX_ARCHIVE_BYTES, "archive").is_ok());
        assert!(ensure_within_limit(MAX_ARCHIVE_BYTES + 1, MAX_ARCHIVE_BYTES, "archive").is_err());
        assert!(ensure_within_limit(
            MAX_UNPACKED_ARCHIVE_BYTES + 1,
            MAX_UNPACKED_ARCHIVE_BYTES,
            "unpacked archive"
        )
        .is_err());
        assert!(
            ensure_within_limit(MAX_WALLET_RPC_BYTES + 1, MAX_WALLET_RPC_BYTES, "executable")
                .is_err()
        );
    }

    #[tokio::test]
    async fn existing_wallet_rpc_requires_a_matching_checksum() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rpc = WalletRpc {
            working_dir: dir.path().to_path_buf(),
        };

        tokio::fs::write(rpc.exec_path(), b"wallet-rpc-binary")
            .await
            .expect("write binary");
        assert!(
            !rpc.verify_exec_checksum().await.expect("verify checksum"),
            "a binary without a checksum must be rejected"
        );

        rpc.write_exec_checksum().await.expect("write checksum");
        assert!(
            rpc.verify_exec_checksum().await.expect("verify checksum"),
            "the checksum written for the binary must verify"
        );

        tokio::fs::write(rpc.exec_path(), b"tampered")
            .await
            .expect("tamper binary");
        assert!(
            !rpc.verify_exec_checksum().await.expect("verify checksum"),
            "a modified binary must fail checksum verification"
        );
    }

    fn extract_host_and_port(address: String) -> (&'static str, u16) {
        let parts: Vec<&str> = address.split(':').collect();

        if parts.len() == 2 {
            let host = parts[0].to_string();
            let port = parts[1].parse::<u16>().unwrap();
            let static_str_host: &'static str = Box::leak(host.into_boxed_str());
            return (static_str_host, port);
        }
        panic!("Could not extract host and port from address: {}", address)
    }

    #[tokio::test]
    async fn test_is_daemon_available_success() {
        let mut server = mockito::Server::new_async().await;

        let _ = server
            .mock("GET", "/get_info")
            .with_status(200)
            .with_body(
                r#"
                {
                    "status": "OK",
                    "synchronized": true,
                    "mainnet": true,
                    "stagenet": false,
                    "testnet": false
                }
                "#,
            )
            .create();

        let (host, port) = extract_host_and_port(server.host_with_port());

        let client = reqwest::Client::new();
        let result = MoneroDaemon::new(host, port, Network::Mainnet)
            .is_available(&client)
            .await;

        assert!(result.is_ok());
        assert!(result.unwrap());
    }

    #[tokio::test]
    async fn test_is_daemon_available_wrong_network_failure() {
        let mut server = mockito::Server::new_async().await;

        let _ = server
            .mock("GET", "/get_info")
            .with_status(200)
            .with_body(
                r#"
                {
                    "status": "OK",
                    "synchronized": true,
                    "mainnet": true,
                    "stagenet": false,
                    "testnet": false
                }
                "#,
            )
            .create();

        let (host, port) = extract_host_and_port(server.host_with_port());

        let client = reqwest::Client::new();
        let result = MoneroDaemon::new(host, port, Network::Stagenet)
            .is_available(&client)
            .await;

        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_is_daemon_available_not_synced_failure() {
        let mut server = mockito::Server::new_async().await;

        let _ = server
            .mock("GET", "/get_info")
            .with_status(200)
            .with_body(
                r#"
                {
                    "status": "OK",
                    "synchronized": false,
                    "mainnet": true,
                    "stagenet": false,
                    "testnet": false
                }
                "#,
            )
            .create();

        let (host, port) = extract_host_and_port(server.host_with_port());

        let client = reqwest::Client::new();
        let result = MoneroDaemon::new(host, port, Network::Mainnet)
            .is_available(&client)
            .await;

        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_is_daemon_available_network_error_failure() {
        let client = reqwest::Client::new();
        let result = MoneroDaemon::new("does.not.exist.com", 18081, Network::Mainnet)
            .is_available(&client)
            .await;

        assert!(result.is_err());
    }

    #[test]
    fn each_download_attempt_uses_its_own_tor_circuit() {
        let first = download_proxy_url("127.0.0.1:9050", 1);
        let second = download_proxy_url("127.0.0.1:9050", 2);
        assert!(first.starts_with("socks5h://"));
        assert!(first.ends_with("@127.0.0.1:9050"));
        assert_ne!(first, second);
    }

    #[tokio::test(start_paused = true)]
    async fn a_download_that_goes_silent_is_reported_as_stalled() {
        let mut silent = futures::stream::pending::<u8>();
        let error = next_or_stall(&mut silent, Duration::from_secs(120)).await.unwrap_err();
        assert!(error.to_string().contains("stalled"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_download_that_keeps_sending_is_not_cut_off() {
        let mut slow = futures::stream::iter(vec![1u8, 2, 3]);
        assert_eq!(next_or_stall(&mut slow, Duration::from_secs(120)).await.unwrap(), Some(1));
    }

    #[test]
    fn a_stall_needs_two_minutes_of_silence_and_there_are_retries() {
        assert!(DOWNLOAD_STALL_TIMEOUT >= Duration::from_secs(120));
        assert!(DOWNLOAD_ATTEMPTS >= 3);
    }
}
