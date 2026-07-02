//! File-based logging setup. The TUI owns stdout/stderr, so all tracing
//! output goes to a rotating file under the OS state directory.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Where log files live: `$XDG_STATE_HOME/ripe` on Linux, falling back to
/// the data directory on platforms without a state dir.
pub fn log_dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "ripe")?;
    Some(
        dirs.state_dir()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| dirs.data_dir().to_path_buf()),
    )
}

/// Initialize tracing to a daily-rotating file, honoring `RUST_LOG`.
///
/// Returns a guard that must be kept alive for the duration of the program;
/// dropping it flushes and stops the background writer.
pub fn init() -> anyhow::Result<WorkerGuard> {
    let dir = log_dir().ok_or_else(|| anyhow::anyhow!("no home directory found"))?;
    init_at(&dir)
}

/// As [`init`], but logging into an explicit directory.
pub fn init_at(dir: &std::path::Path) -> anyhow::Result<WorkerGuard> {
    std::fs::create_dir_all(dir)?;
    let appender = tracing_appender::rolling::daily(dir, "ripe.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(writer)
        .with_ansi(false)
        .init();
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_writes_to_a_file_in_the_given_dir() {
        let dir = std::env::temp_dir().join(format!("ripe-log-test-{}", std::process::id()));
        // Env filter defaults to error-only without RUST_LOG; error! must land.
        let guard = init_at(&dir).unwrap();
        tracing::error!("logging smoke test");
        drop(guard); // flush

        let mut entries = std::fs::read_dir(&dir).unwrap();
        let file = entries.next().expect("a log file exists").unwrap();
        let content = std::fs::read_to_string(file.path()).unwrap();
        assert!(content.contains("logging smoke test"), "{content}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
