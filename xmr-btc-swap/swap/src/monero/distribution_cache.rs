//! A kept copy of the Monero ringct output distribution.
//!
//! Decoy selection asks the daemon for `get_output_distribution.bin` over the
//! whole chain. On testnet that is about 17 MB, and over an onion daemon it
//! takes around two minutes (measured 2026-11-08: 8.4 MB in 52 s), which made
//! every redeem slow and, under the old 60 s request timeout, fail.
//!
//! The distribution only ever grows at the tip, so an earlier copy stays valid
//! except for a reorg of the last few blocks. This module keeps the last answer
//! and, on the next request, asks the daemon only for the tail: the final
//! [`OVERLAP`] blocks of the kept copy up to the requested height. The overlap is
//! compared with the kept copy; any disagreement discards the copy and the
//! caller falls back to the full download. The reply handed to the library is
//! the daemon's own tail reply with its `distribution` string and `start_height`
//! rewritten, so everything else in it is the daemon's.
//!
//! Only the request shape that `monero-daemon-rpc` sends is recognized
//! (`from_height` first, then `to_height`, cumulative, uncompressed). Any other
//! shape bypasses the cache.

use monero_interface::InterfaceError;
use sha2::{Digest as _, Sha256};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const ROUTE: &str = "get_output_distribution.bin";

/// Blocks at the end of the kept copy that are fetched again and compared.
pub const OVERLAP: u64 = 100;

const FILE_MAGIC: &[u8; 8] = b"SHGDIST1";

const EPEE_HEADER: [u8; 9] = [0x01, 0x11, 0x01, 0x01, 0x01, 0x01, 0x02, 0x01, 0x01];

/// `values[i]` is the cumulative output count at height `start_height + i`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Distribution {
    pub start_height: u64,
    pub values: Vec<u64>,
}

impl Distribution {
    fn last_height(&self) -> Option<u64> {
        let len = u64::try_from(self.values.len()).ok()?;
        self.start_height.checked_add(len.checked_sub(1)?)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub from: u64,
    pub to: u64,
    /// Where the 8 bytes of `from_height` sit in the request body.
    from_offset: usize,
}

/// Recognize the exact request `monero-daemon-rpc` builds for the ringct
/// distribution. Anything else returns `None` and bypasses the cache.
pub fn parse_request(body: &[u8]) -> Option<Request> {
    const FROM_KEY: &[u8] = b"\x0bfrom_height\x05";
    const TO_KEY: &[u8] = b"\x09to_height\x05";
    // header, version, then 5 fields (varint 5 << 2)
    if body.get(..EPEE_HEADER.len())? != EPEE_HEADER || *body.get(EPEE_HEADER.len())? != 5 << 2 {
        return None;
    }
    let mut at = EPEE_HEADER.len() + 1;
    if body.get(at..at + FROM_KEY.len())? != FROM_KEY {
        return None;
    }
    at += FROM_KEY.len();
    let from_offset = at;
    let from = u64::from_le_bytes(body.get(at..at + 8)?.try_into().ok()?);
    at += 8;
    if body.get(at..at + TO_KEY.len())? != TO_KEY {
        return None;
    }
    at += TO_KEY.len();
    let to = u64::from_le_bytes(body.get(at..at + 8)?.try_into().ok()?);
    let rest = body.get(at + 8..)?;
    let contains = |needle: &[u8]| rest.windows(needle.len()).any(|window| window == needle);
    // cumulative = true, compress = false: the reply is a plain u64 array
    if !contains(b"\x0acumulative\x0b\x01") || !contains(b"\x08compress\x0b\x00") {
        return None;
    }
    Some(Request {
        from,
        to,
        from_offset,
    })
}

fn read_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let width = match first & 0b11 {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let raw = bytes.get(..width)?;
    let mut buf = [0u8; 8];
    buf[..width].copy_from_slice(raw);
    Some((u64::from_le_bytes(buf) >> 2, width))
}

fn write_varint(value: u64) -> Option<Vec<u8>> {
    let (mark, width) = if value < 1 << 6 {
        (0u64, 1)
    } else if value < 1 << 14 {
        (1, 2)
    } else if value < 1 << 30 {
        (2, 4)
    } else if value < 1 << 62 {
        (3, 8)
    } else {
        return None;
    };
    Some(((value << 2) | mark).to_le_bytes()[..width].to_vec())
}

/// The parts of a `get_output_distribution.bin` reply that matter here.
struct ParsedReply {
    /// Offset of the varint that holds the length of the `distribution` string.
    varint_at: usize,
    data_at: usize,
    data_len: usize,
    /// Offset of the 8 bytes of `start_height`.
    start_height_at: usize,
    start_height: u64,
    values: Vec<u64>,
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| position + from)
}

fn parse_reply(reply: &[u8]) -> Option<ParsedReply> {
    const DISTRIBUTION_KEY: &[u8] = b"\x0cdistribution\x0a";
    const START_KEY: &[u8] = b"\x0cstart_height\x05";
    let key_at = find(reply, DISTRIBUTION_KEY, 0)?;
    let varint_at = key_at + DISTRIBUTION_KEY.len();
    let (data_len, varint_width) = read_varint(reply.get(varint_at..)?)?;
    let data_len = usize::try_from(data_len).ok()?;
    let data_at = varint_at + varint_width;
    let data = reply.get(data_at..data_at.checked_add(data_len)?)?;
    if data_len % 8 != 0 {
        return None;
    }
    let start_key_at = find(reply, START_KEY, data_at + data_len)?;
    let start_height_at = start_key_at + START_KEY.len();
    let start_height = u64::from_le_bytes(reply.get(start_height_at..start_height_at + 8)?.try_into().ok()?);
    let values = data
        .chunks_exact(8)
        .map(|chunk| u64::from_le_bytes(chunk.try_into().expect("chunks of 8")))
        .collect();
    Some(ParsedReply {
        varint_at,
        data_at,
        data_len,
        start_height_at,
        start_height,
        values,
    })
}

/// Rewrite `reply` so that it carries `distribution` instead of its own
/// distribution string, and `start_height` accordingly.
fn rewrite_reply(reply: &[u8], parsed: &ParsedReply, distribution: &Distribution) -> Option<Vec<u8>> {
    let mut data = Vec::with_capacity(distribution.values.len() * 8);
    for value in &distribution.values {
        data.extend_from_slice(&value.to_le_bytes());
    }
    let mut out = Vec::with_capacity(reply.len() + data.len());
    out.extend_from_slice(&reply[..parsed.varint_at]);
    out.extend_from_slice(&write_varint(u64::try_from(data.len()).ok()?)?);
    out.extend_from_slice(&data);
    let after_data = parsed.data_at + parsed.data_len;
    // `start_height` follows the data in the daemon's reply; patch it in place.
    let start_in_rest = parsed.start_height_at.checked_sub(after_data)?;
    let mut rest = reply[after_data..].to_vec();
    rest[start_in_rest..start_in_rest + 8].copy_from_slice(&distribution.start_height.to_le_bytes());
    out.extend_from_slice(&rest);
    Some(out)
}

/// Merge the kept copy with a tail reply that started at `tail_from`. Returns
/// the reply to hand to the library and the new kept copy, or `None` if the tail
/// does not agree with what is kept.
pub fn splice(kept: &Distribution, tail_reply: &[u8], tail_from: u64) -> Option<(Vec<u8>, Distribution)> {
    let tail = parse_reply(tail_reply)?;
    if tail.start_height != tail_from || tail_from < kept.start_height {
        return None;
    }
    let keep_len = usize::try_from(tail_from - kept.start_height).ok()?;
    let overlap = kept.values.get(keep_len..)?;
    if tail.values.len() < overlap.len() || tail.values[..overlap.len()] != *overlap {
        return None;
    }
    let mut values = kept.values[..keep_len].to_vec();
    values.extend_from_slice(&tail.values);
    if values.windows(2).any(|pair| pair[1] < pair[0]) {
        return None;
    }
    let merged = Distribution {
        start_height: kept.start_height,
        values,
    };
    let reply = rewrite_reply(tail_reply, &tail, &merged)?;
    Some((reply, merged))
}

/// The kept copy, in memory and (once a path is set) on disk.
#[derive(Debug, Default)]
pub struct DistributionCache {
    path: Mutex<Option<PathBuf>>,
    kept: Mutex<Option<Distribution>>,
    loaded: Mutex<bool>,
}

impl DistributionCache {
    pub fn set_path(&self, path: PathBuf) {
        *self.path.lock().expect("cache path lock") = Some(path);
        *self.loaded.lock().expect("cache loaded lock") = false;
    }

    fn enabled(&self) -> bool {
        self.path.lock().expect("cache path lock").is_some()
    }

    fn load(&self) -> Option<Distribution> {
        let path = self.path.lock().expect("cache path lock").clone()?;
        {
            let mut loaded = self.loaded.lock().expect("cache loaded lock");
            if !*loaded {
                *loaded = true;
                *self.kept.lock().expect("cache lock") = read_file(&path);
            }
        }
        self.kept.lock().expect("cache lock").clone()
    }

    fn store(&self, distribution: Distribution) {
        if let Some(path) = self.path.lock().expect("cache path lock").clone() {
            if let Err(error) = write_file(&path, &distribution) {
                tracing::warn!(%error, path = %path.display(), "Could not save the output distribution cache");
            }
        }
        *self.kept.lock().expect("cache lock") = Some(distribution);
    }

    fn clear(&self) {
        *self.kept.lock().expect("cache lock") = None;
        if let Some(path) = self.path.lock().expect("cache path lock").clone() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn encode_file(distribution: &Distribution) -> Vec<u8> {
    let mut body = Vec::with_capacity(distribution.values.len() * 8 + 16);
    body.extend_from_slice(&distribution.start_height.to_le_bytes());
    body.extend_from_slice(&(distribution.values.len() as u64).to_le_bytes());
    for value in &distribution.values {
        body.extend_from_slice(&value.to_le_bytes());
    }
    let digest = Sha256::digest(&body);
    let mut out = Vec::with_capacity(FILE_MAGIC.len() + 32 + body.len());
    out.extend_from_slice(FILE_MAGIC);
    out.extend_from_slice(&digest);
    out.extend_from_slice(&body);
    out
}

fn decode_file(bytes: &[u8]) -> Option<Distribution> {
    let rest = bytes.strip_prefix(FILE_MAGIC.as_slice())?;
    let (digest, body) = rest.split_at_checked(32)?;
    if Sha256::digest(body).as_slice() != digest || body.len() < 16 {
        return None;
    }
    let start_height = u64::from_le_bytes(body[..8].try_into().ok()?);
    let count = usize::try_from(u64::from_le_bytes(body[8..16].try_into().ok()?)).ok()?;
    let data = &body[16..];
    if data.len() != count.checked_mul(8)? {
        return None;
    }
    let values = data
        .chunks_exact(8)
        .map(|chunk| u64::from_le_bytes(chunk.try_into().expect("chunks of 8")))
        .collect();
    Some(Distribution { start_height, values })
}

fn read_file(path: &Path) -> Option<Distribution> {
    decode_file(&std::fs::read(path).ok()?)
}

fn write_file(path: &Path, distribution: &Distribution) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, encode_file(distribution))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&temp, path)
}

/// Answer a `get_output_distribution.bin` request, asking the daemon (through
/// `fetch`) for as little as possible.
pub async fn distribution_post<F, Fut>(
    cache: &DistributionCache,
    body: Vec<u8>,
    fetch: F,
) -> Result<Vec<u8>, InterfaceError>
where
    F: Fn(Vec<u8>) -> Fut,
    Fut: Future<Output = Result<Vec<u8>, InterfaceError>>,
{
    let request = match parse_request(&body) {
        Some(request) if request.from == 0 && cache.enabled() => request,
        _ => return fetch(body).await,
    };

    if let Some(kept) = cache.load() {
        match kept.last_height() {
            // The kept copy is ahead of this request (a lagging daemon): serve nothing from it.
            Some(last) if last > request.to => return fetch(body).await,
            Some(last) => {
                let tail_from = last.saturating_sub(OVERLAP).max(kept.start_height);
                let mut tail_body = body.clone();
                tail_body[request.from_offset..request.from_offset + 8]
                    .copy_from_slice(&tail_from.to_le_bytes());
                let started = std::time::Instant::now();
                match fetch(tail_body).await {
                    Ok(tail_reply) => match splice(&kept, &tail_reply, tail_from) {
                        Some((reply, merged)) => {
                            tracing::info!(
                                tail_from,
                                tail_bytes = tail_reply.len(),
                                elapsed_ms = started.elapsed().as_millis() as u64,
                                "Output distribution completed from the kept copy"
                            );
                            cache.store(merged);
                            return Ok(reply);
                        }
                        None => {
                            tracing::warn!(
                                "The daemon's output distribution disagrees with the kept copy; discarding it and downloading in full"
                            );
                            cache.clear();
                        }
                    },
                    Err(error) => {
                        tracing::warn!(%error, "Could not fetch the output distribution tail; downloading in full");
                    }
                }
            }
            None => cache.clear(),
        }
    }

    let reply = fetch(body).await?;
    if let Some(parsed) = parse_reply(&reply) {
        if parsed.values.windows(2).all(|pair| pair[0] <= pair[1]) {
            cache.store(Distribution {
                start_height: parsed.start_height,
                values: parsed.values,
            });
        }
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use monero_daemon_rpc::{HttpTransport, MoneroDaemon};
    use monero_interface::ProvidesUnvalidatedDecoys as _;
    use std::sync::{Arc, Mutex};

    const FIRST_HEIGHT: u64 = 10;

    /// A daemon with a chain of cumulative output counts, answering
    /// `get_output_distribution.bin` in the layout a real daemon uses (captured
    /// 2026-11-08), and recording every `(from, to)` it is asked for.
    #[derive(Clone)]
    struct FakeDaemon {
        chain: Arc<Mutex<Vec<u64>>>,
        asked: Arc<Mutex<Vec<(u64, u64)>>>,
        cache: Arc<DistributionCache>,
    }

    fn key(name: &str) -> Vec<u8> {
        let mut out = vec![name.len() as u8];
        out.extend_from_slice(name.as_bytes());
        out
    }

    /// Built field by field from the captured reply, not with the code under test.
    fn reply(start_height: u64, values: &[u64]) -> Vec<u8> {
        let mut out = EPEE_HEADER.to_vec();
        out.push(5 << 2); // 5 fields
        out.extend(key("credits"));
        out.push(0x05);
        out.extend(0u64.to_le_bytes());
        out.extend(key("distributions"));
        out.push(0x0c | 0x80); // object array
        out.push(1 << 2); // one element
        out.push(6 << 2); // six fields
        out.extend(key("amount"));
        out.push(0x05);
        out.extend(0u64.to_le_bytes());
        out.extend(key("base"));
        out.push(0x05);
        out.extend(7u64.to_le_bytes());
        out.extend(key("binary"));
        out.extend([0x0b, 1]);
        out.extend(key("compress"));
        out.extend([0x0b, 0]);
        out.extend(key("distribution"));
        out.push(0x0a);
        out.extend(write_varint(values.len() as u64 * 8).unwrap());
        for value in values {
            out.extend(value.to_le_bytes());
        }
        out.extend(key("start_height"));
        out.push(0x05);
        out.extend(start_height.to_le_bytes());
        out.extend(key("status"));
        out.push(0x0a);
        out.extend([2 << 2, b'O', b'K']);
        out.extend(key("top_hash"));
        out.push(0x0a);
        out.push(0);
        out.extend(key("untrusted"));
        out.extend([0x0b, 0]);
        out
    }

    impl FakeDaemon {
        fn new(blocks: usize, cache_path: Option<PathBuf>) -> Self {
            let cache = Arc::new(DistributionCache::default());
            if let Some(path) = cache_path {
                cache.set_path(path);
            }
            Self {
                chain: Arc::new(Mutex::new(
                    (0..blocks).map(|height| 1000 + height as u64 * 3).collect(),
                )),
                asked: Arc::default(),
                cache,
            }
        }

        fn tip(&self) -> u64 {
            FIRST_HEIGHT + self.chain.lock().unwrap().len() as u64 - 1
        }

        fn grow(&self, blocks: usize) {
            let mut chain = self.chain.lock().unwrap();
            for _ in 0..blocks {
                let next = chain.last().unwrap() + 3;
                chain.push(next);
            }
        }

        fn truth(&self) -> Vec<u64> {
            self.chain.lock().unwrap().clone()
        }

        fn serve(&self, body: &[u8]) -> Result<Vec<u8>, InterfaceError> {
            let request = parse_request(body).expect("request recognized");
            self.asked.lock().unwrap().push((request.from, request.to));
            let start = request.from.max(FIRST_HEIGHT);
            let chain = self.chain.lock().unwrap();
            let values = &chain[(start - FIRST_HEIGHT) as usize..=(request.to - FIRST_HEIGHT) as usize];
            Ok(reply(start, values))
        }
    }

    impl HttpTransport for FakeDaemon {
        fn post(
            &self,
            route: &str,
            body: Vec<u8>,
            _limit: Option<usize>,
        ) -> impl Send + Future<Output = Result<Vec<u8>, InterfaceError>> {
            let this = self.clone();
            let route = route.to_string();
            async move {
                if route == "json_rpc" {
                    // what a daemon that rejects batches answers; MoneroDaemon::new accepts it
                    return Ok(br#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error"},"id":0}"#.to_vec());
                }
                assert_eq!(route, ROUTE);
                let server = this.clone();
                distribution_post(&this.cache, body, move |body| {
                    let server = server.clone();
                    async move { server.serve(&body) }
                })
                .await
            }
        }
    }

    async fn decoy_distribution(daemon: &FakeDaemon) -> Vec<u64> {
        let library = MoneroDaemon::new(daemon.clone()).await.unwrap();
        let tip = usize::try_from(daemon.tip()).unwrap();
        let values = library.ringct_output_distribution(..=tip).await.unwrap();
        // the library returns heights from the daemon's first block
        values
    }

    #[tokio::test]
    async fn the_first_request_downloads_everything_and_the_next_only_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = FakeDaemon::new(1000, Some(dir.path().join("dist.bin")));

        assert_eq!(decoy_distribution(&daemon).await, daemon.truth());
        assert_eq!(daemon.asked.lock().unwrap().as_slice(), &[(0, daemon.tip())]);

        daemon.asked.lock().unwrap().clear();
        let previous_tip = daemon.tip();
        daemon.grow(7);
        assert_eq!(decoy_distribution(&daemon).await, daemon.truth());
        assert_eq!(
            daemon.asked.lock().unwrap().as_slice(),
            &[(previous_tip - OVERLAP, daemon.tip())],
            "only the last {OVERLAP} blocks of the kept copy up to the tip are fetched"
        );
    }

    #[tokio::test]
    async fn a_second_process_reads_the_copy_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dist.bin");
        let first = FakeDaemon::new(500, Some(path.clone()));
        decoy_distribution(&first).await;

        let second = FakeDaemon::new(500, Some(path));
        second.grow(3);
        assert_eq!(decoy_distribution(&second).await, second.truth());
        assert_eq!(second.asked.lock().unwrap().len(), 1);
        assert_ne!(second.asked.lock().unwrap()[0].0, 0);
    }

    #[tokio::test]
    async fn a_reorg_inside_the_overlap_falls_back_to_the_full_download() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = FakeDaemon::new(600, Some(dir.path().join("dist.bin")));
        decoy_distribution(&daemon).await;

        // the last 5 blocks are replaced by a different history
        {
            let mut chain = daemon.chain.lock().unwrap();
            let len = chain.len();
            for value in &mut chain[len - 5..] {
                *value += 1;
            }
        }
        daemon.grow(2);
        daemon.asked.lock().unwrap().clear();

        assert_eq!(decoy_distribution(&daemon).await, daemon.truth());
        let asked = daemon.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 2, "tail, then full: {asked:?}");
        assert_eq!(asked[1].0, 0);
    }

    #[tokio::test]
    async fn without_a_path_every_request_goes_to_the_daemon_unchanged() {
        let daemon = FakeDaemon::new(300, None);
        decoy_distribution(&daemon).await;
        decoy_distribution(&daemon).await;
        assert_eq!(daemon.asked.lock().unwrap().len(), 2);
        assert!(daemon.asked.lock().unwrap().iter().all(|(from, _)| *from == 0));
    }

    #[tokio::test]
    async fn a_kept_copy_ahead_of_the_request_is_not_used() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = FakeDaemon::new(400, Some(dir.path().join("dist.bin")));
        decoy_distribution(&daemon).await;
        // a lagging daemon: a shorter chain than the kept copy
        let lagging = FakeDaemon::new(350, None);
        let lagging = FakeDaemon { cache: daemon.cache.clone(), ..lagging };
        assert_eq!(decoy_distribution(&lagging).await, lagging.truth());
        assert_eq!(lagging.asked.lock().unwrap().as_slice(), &[(0, lagging.tip())]);
    }

    #[test]
    fn the_request_the_library_sends_is_recognized() {
        // the exact bytes monero-daemon-rpc builds for from 0, to 2246481
        let mut body = EPEE_HEADER.to_vec();
        body.push(5 << 2);
        body.extend(key("from_height"));
        body.push(0x05);
        body.extend(0u64.to_le_bytes());
        body.extend(key("to_height"));
        body.push(0x05);
        body.extend(2246481u64.to_le_bytes());
        body.extend(key("cumulative"));
        body.extend([0x0b, 1]);
        body.extend(key("compress"));
        body.extend([0x0b, 0]);
        body.extend(key("amounts"));
        body.extend([0x88, 1 << 2, 0]);
        let request = parse_request(&body).unwrap();
        assert_eq!((request.from, request.to), (0, 2246481));

        let mut compressed = body.clone();
        let at = compressed.len() - 12; // the compress value, before the 8-byte `amounts` key and its 3 value bytes
        compressed[at] = 1; // compress = true
        assert!(parse_request(&compressed).is_none());
        assert!(parse_request(&body[..20]).is_none());
    }

    #[test]
    fn varints_round_trip_at_every_width() {
        for value in [0u64, 63, 64, 16_383, 16_384, 1 << 29, 1 << 30, 1 << 40] {
            let bytes = write_varint(value).unwrap();
            assert_eq!(read_varint(&bytes), Some((value, bytes.len())));
        }
    }

    #[test]
    fn the_saved_file_rejects_corruption() {
        let distribution = Distribution { start_height: 5, values: vec![1, 2, 3] };
        let mut bytes = encode_file(&distribution);
        assert_eq!(decode_file(&bytes), Some(distribution));
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        assert_eq!(decode_file(&bytes), None);
        assert_eq!(decode_file(b"nonsense"), None);
    }

    #[test]
    fn a_real_daemon_reply_is_parsed() {
        // captured from a testnet daemon on 2026-11-08: from 2246000, 21 blocks
        let real = hex_to_bytes(REAL_REPLY_HEX);
        let parsed = parse_reply(&real).expect("real reply parsed");
        assert_eq!(parsed.start_height, 2246000);
        assert_eq!(parsed.values.len(), 21);
        let rewritten = rewrite_reply(
            &real,
            &parsed,
            &Distribution { start_height: 2245900, values: [vec![1; 100], parsed.values.clone()].concat() },
        )
        .unwrap();
        let again = parse_reply(&rewritten).unwrap();
        assert_eq!(again.start_height, 2245900);
        assert_eq!(again.values.len(), 121);
    }

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        let hex: String = hex.split_whitespace().collect();
        (0..hex.len() / 2)
            .map(|index| u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap())
            .collect()
    }

    const REAL_REPLY_HEX: &str = "0111010101010201011407637265646974730500000000000000000d646973747269627574696f6e738c041806616d6f756e7405000000000000000004626173650517019b00000000000662696e6172790b0108636f6d70726573730b000c646973747269627574696f6e0aa10218019b000000000019019b00000000001a019b00000000001b019b000000000022019b000000000023019b000000000024019b000000000025019b000000000026019b000000000027019b000000000028019b000000000029019b00000000002e019b000000000033019b000000000034019b000000000035019b000000000036019b000000000037019b000000000038019b000000000039019b00000000003a019b00000000000c73746172745f686569676874057045220000000000067374617475730a084f4b08746f705f686173680a0009756e747275737465640b00";
}
