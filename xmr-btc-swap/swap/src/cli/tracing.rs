use anyhow::Result;
use std::io::{self, Write};
use std::option::Option::Some;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use time::format_description::well_known::Rfc3339;
use tracing::subscriber::set_global_default;
use tracing::{Event, Level, Subscriber};
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::fmt::format::{DefaultFields, Format, JsonFields};
use tracing_subscriber::fmt::time::UtcTime;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{fmt, EnvFilter, FmtSubscriber, Layer, Registry};
use uuid::Uuid;

/// The live swap's file target. The first `init(Some(swap_id))` installs the
/// global subscriber with a writer that reads this target; later swaps replace
/// the target instead of calling `set_global_default` again.
struct SwapLogState {
    appender: RwLock<Option<NonBlocking>>,
    guard: Mutex<Option<WorkerGuard>>,
}

impl SwapLogState {
    fn set_target(&self, appender: NonBlocking, guard: WorkerGuard) {
        *self.appender.write().unwrap_or_else(|e| e.into_inner()) = Some(appender);
        // Dropping the previous guard flushes its worker before the new
        // target becomes active for subsequent writes.
        *self.guard.lock().unwrap_or_else(|e| e.into_inner()) = Some(guard);
    }
}

static SWAP_LOG_STATE: OnceLock<Arc<SwapLogState>> = OnceLock::new();

#[derive(Clone)]
struct SharedWriter {
    state: Arc<SwapLogState>,
}

impl<'a> MakeWriter<'a> for SharedWriter {
    type Writer = SharedWriterGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        SharedWriterGuard {
            state: &self.state,
        }
    }
}

struct SharedWriterGuard<'a> {
    state: &'a SwapLogState,
}

impl Write for SharedWriterGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let appender = self
            .state
            .appender
            .read()
            .unwrap_or_else(|e| e.into_inner());
        match appender.as_ref() {
            Some(appender) => appender.make_writer().write(buf),
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let appender = self
            .state
            .appender
            .read()
            .unwrap_or_else(|e| e.into_inner());
        match appender.as_ref() {
            Some(appender) => appender.make_writer().flush(),
            None => Ok(()),
        }
    }
}

fn create_swap_appender(dir: &Path, swap_id: Uuid) -> (NonBlocking, WorkerGuard) {
    let appender = tracing_appender::rolling::never(dir, format!("swap-{}.log", swap_id));
    tracing_appender::non_blocking(appender)
}

static LOG_INIT_LOCK: Mutex<()> = Mutex::new(());

/// What a swap's log file (and the terminal) records. The seller list runs during and after a
/// swap and dials every known seller over Tor; its per-peer chatter (an unreachable seller is
/// routine) is kept to info so it does not flood a finished swap's file, while its warnings,
/// such as an unreachable rendezvous point, still get through.
pub(crate) const SWAP_LOG_FILTER: &str =
    "swap=debug,swap::cli::list_sellers=info,bitcoin_wallet=info,atomicswap=info,electrum_pool=info,monero_wallet_ng=info";

pub fn init(debug: bool, json: bool, dir: impl AsRef<Path>, swap_id: Option<Uuid>) -> Result<()> {
    let _init_guard = LOG_INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(swap_id) = swap_id {
        let (appender, guard) = create_swap_appender(dir.as_ref(), swap_id);

        // A process handles more than one swap over its lifetime (resume then
        // a new buy, or several sequential buys). `set_global_default` can
        // only run once, so later swaps switch the writer target instead of
        // silently keeping the first swap's file.
        if let Some(state) = SWAP_LOG_STATE.get() {
            state.set_target(appender, guard);
            tracing::info!("Logging switched to {}", dir.as_ref().display());
            return Ok(());
        }

        let state = Arc::new(SwapLogState {
            appender: RwLock::new(Some(appender)),
            guard: Mutex::new(Some(guard)),
        });
        // The app's own crate (`atomicswap`), the Electrum pool and the Monero
        // wallet were missing here, so their timings, retries and "session is
        // dead" warnings never reached the swap log (swaps f7a162f7, 139e7672).
        let level_filter = EnvFilter::try_new(SWAP_LOG_FILTER)?;

        let registry = Registry::default().with(level_filter);

        let file_logger = registry.with(
            fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .json()
                .with_writer(SharedWriter { state: state.clone() }),
        );

        if json && debug {
            set_global_default(file_logger.with(debug_json_terminal_printer()))?;
        } else if json && !debug {
            set_global_default(file_logger.with(info_json_terminal_printer()))?;
        } else if !json && debug {
            set_global_default(file_logger.with(debug_terminal_printer()))?;
        } else {
            set_global_default(file_logger.with(info_terminal_printer()))?;
        }
        let _ = SWAP_LOG_STATE.set(state);
    } else {
        let level = if debug { Level::DEBUG } else { Level::INFO };
        let is_terminal = atty::is(atty::Stream::Stderr);

        let builder = FmtSubscriber::builder()
            .with_env_filter(format!("swap={}", level))
            .with_writer(std::io::stderr)
            .with_ansi(is_terminal)
            .with_timer(UtcTime::rfc_3339())
            .with_target(false);

        if json {
            builder.json().init();
        } else {
            builder.init();
        }
    };

    tracing::info!("Logging initialized to {}", dir.as_ref().display());
    Ok(())
}

pub struct StdErrPrinter<L> {
    inner: L,
    level: Level,
}

type StdErrLayer<S, T> = tracing_subscriber::fmt::Layer<
    S,
    DefaultFields,
    Format<tracing_subscriber::fmt::format::Full, T>,
    fn() -> std::io::Stderr,
>;

type StdErrJsonLayer<S, T> = tracing_subscriber::fmt::Layer<
    S,
    JsonFields,
    Format<tracing_subscriber::fmt::format::Json, T>,
    fn() -> std::io::Stderr,
>;

fn debug_terminal_printer<S>() -> StdErrPrinter<StdErrLayer<S, UtcTime<Rfc3339>>> {
    let is_terminal = atty::is(atty::Stream::Stderr);
    StdErrPrinter {
        inner: fmt::layer()
            .with_ansi(is_terminal)
            .with_target(false)
            .with_timer(UtcTime::rfc_3339())
            .with_writer(std::io::stderr),
        level: Level::DEBUG,
    }
}

fn debug_json_terminal_printer<S>() -> StdErrPrinter<StdErrJsonLayer<S, UtcTime<Rfc3339>>> {
    let is_terminal = atty::is(atty::Stream::Stderr);
    StdErrPrinter {
        inner: fmt::layer()
            .with_ansi(is_terminal)
            .with_target(false)
            .with_timer(UtcTime::rfc_3339())
            .json()
            .with_writer(std::io::stderr),
        level: Level::DEBUG,
    }
}

fn info_terminal_printer<S>() -> StdErrPrinter<StdErrLayer<S, ()>> {
    let is_terminal = atty::is(atty::Stream::Stderr);
    StdErrPrinter {
        inner: fmt::layer()
            .with_ansi(is_terminal)
            .with_target(false)
            .with_level(false)
            .without_time()
            .with_writer(std::io::stderr),
        level: Level::INFO,
    }
}

fn info_json_terminal_printer<S>() -> StdErrPrinter<StdErrJsonLayer<S, ()>> {
    let is_terminal = atty::is(atty::Stream::Stderr);
    StdErrPrinter {
        inner: fmt::layer()
            .with_ansi(is_terminal)
            .with_target(false)
            .with_level(false)
            .without_time()
            .json()
            .with_writer(std::io::stderr),
        level: Level::INFO,
    }
}

impl<L, S> Layer<S> for StdErrPrinter<L>
where
    L: 'static + Layer<S>,
    S: Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        if self.level.ge(event.metadata().level()) {
            self.inner.on_event(event, ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write as _;
    use std::time::{Duration, Instant};
    use tracing_subscriber::fmt::MakeWriter as _;

    /// Reproduces the multi-swap log bug: the first target must receive the
    /// first swap's lines, and a later target must receive the next swap's
    /// lines without either file leaking into the other.
    #[test]
    fn shared_writer_switches_swap_log_targets() {
        let dir = std::env::temp_dir().join(format!("outpost-swap-log-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let (first, first_guard) =
            tracing_appender::non_blocking(tracing_appender::rolling::never(&dir, "first.log"));
        let state = Arc::new(SwapLogState {
            appender: RwLock::new(Some(first)),
            guard: Mutex::new(Some(first_guard)),
        });
        let writer = SharedWriter {
            state: state.clone(),
        };

        writer.make_writer().write_all(b"first\n").unwrap();
        writer.make_writer().flush().unwrap();
        wait_for_text(&dir.join("first.log"), "first");

        let (second, second_guard) =
            tracing_appender::non_blocking(tracing_appender::rolling::never(&dir, "second.log"));
        state.set_target(second, second_guard);

        writer.make_writer().write_all(b"second\n").unwrap();
        writer.make_writer().flush().unwrap();
        wait_for_text(&dir.join("second.log"), "second");

        assert_eq!(
            fs::read_to_string(dir.join("first.log")).unwrap(),
            "first\n"
        );
        assert_eq!(
            fs::read_to_string(dir.join("second.log")).unwrap(),
            "second\n"
        );

        fs::remove_dir_all(&dir).ok();
    }

    fn wait_for_text(path: &Path, expected: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if fs::read_to_string(path)
                .map(|contents| contents.contains(expected))
                .unwrap_or(false)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {expected:?} in {path:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The swap log keeps the seller list's per-peer chatter and the wallet's fee-floor note
    /// out, but still records the rendezvous warning and the swap's own debug lines.
    #[test]
    fn the_swap_log_filter_drops_seller_chatter_and_the_fee_floor() {
        #[derive(Clone)]
        struct Buf(Arc<Mutex<Vec<u8>>>);
        impl Write for Buf {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let captured = Arc::new(Mutex::new(Vec::new()));
        let buf = Buf(captured.clone());
        let subscriber = Registry::default()
            .with(EnvFilter::try_new(SWAP_LOG_FILTER).unwrap())
            .with(fmt::layer().with_ansi(false).with_writer(move || buf.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(target: "swap::cli::list_sellers", "Failed to connect to peer");
            tracing::warn!(target: "swap::cli::list_sellers", "Failed to connect to rendezvous point");
            tracing::debug!(target: "bitcoin_wallet::wallet", "Recommended fee rate is below");
            tracing::debug!(target: "swap::protocol_v5::bob", "swap detail");
        });
        let out = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
        assert!(!out.contains("Failed to connect to peer"), "{out}");
        assert!(out.contains("Failed to connect to rendezvous point"), "{out}");
        assert!(!out.contains("Recommended fee rate"), "{out}");
        assert!(out.contains("swap detail"), "{out}");
    }
}
