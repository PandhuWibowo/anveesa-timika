//! Operational logging: what the service is doing (startup, storage, raft,
//! seal events, failures). Distinct from the *audit* log (`audit.rs`), which
//! records who did what.
//!
//! Always to stdout (for `docker logs` / kubectl), and optionally to rotating
//! files on a volume so the history survives container restarts:
//!
//!   LOG_LEVEL        filter, e.g. `info` or `info,openraft=warn` (falls back to RUST_LOG)
//!   LOG_FORMAT       stdout format: `text` (default) or `json`
//!   LOG_DIR          enable file logging into this directory (JSON lines, always)
//!   LOG_ROTATION     `daily` (default) · `hourly` · `never`
//!   LOG_MAX_FILES    rotated files to keep (default 14); older ones are deleted
//!   LOG_FILE_PREFIX  file name prefix (default `timika-{instance}`)

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Layer};

/// Keep this alive for the life of the process: dropping it stops the
/// background file writer (and flushes what's buffered).
pub struct LogGuard {
    _file: Option<WorkerGuard>,
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

pub fn init(instance: &str) -> anyhow::Result<LogGuard> {
    let filter = || {
        env("LOG_LEVEL")
            .or_else(|| env("RUST_LOG"))
            .map(EnvFilter::new)
            .unwrap_or_else(|| EnvFilter::new("info,anveesa_timika_backend=debug,openraft=warn"))
    };

    let stdout_json = env("LOG_FORMAT").is_some_and(|f| f.eq_ignore_ascii_case("json"));
    let stdout = if stdout_json {
        fmt::layer().json().with_current_span(false).with_filter(filter()).boxed()
    } else {
        fmt::layer().with_filter(filter()).boxed()
    };

    let (file_layer, guard) = match env("LOG_DIR") {
        Some(dir) => {
            std::fs::create_dir_all(&dir)?;
            let rotation = match env("LOG_ROTATION").as_deref() {
                Some("hourly") => Rotation::HOURLY,
                Some("never") => Rotation::NEVER,
                _ => Rotation::DAILY,
            };
            let prefix = env("LOG_FILE_PREFIX")
                .unwrap_or_else(|| "timika-{instance}".into())
                .replace("{instance}", &sanitize(instance));
            let appender = RollingFileAppender::builder()
                .rotation(rotation)
                .filename_prefix(prefix)
                .filename_suffix("log")
                .max_log_files(env("LOG_MAX_FILES").and_then(|v| v.parse().ok()).unwrap_or(14))
                .build(&dir)?;
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = fmt::layer()
                .json()
                .with_current_span(false)
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter())
                .boxed();
            (Some(layer), Some(guard))
        }
        None => (None, None),
    };

    tracing_subscriber::registry().with(stdout).with(file_layer).init();
    if let Some(dir) = env("LOG_DIR") {
        tracing::info!("file logging enabled in {dir}");
    }
    Ok(LogGuard { _file: guard })
}

/// File-name-safe version of an instance id.
pub fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect()
}
