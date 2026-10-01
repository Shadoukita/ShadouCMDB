//! Structured logs: one JSON object per line,
//! to stdout or, with `--log-file`, appended to a file.

use std::fs::OpenOptions;
use std::path::Path;
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;

/// Maps the LOG_LEVEL values from .env.example onto tracing levels.
fn filter(level: &str) -> EnvFilter {
    let level = match level {
        "fatal" | "error" => "error",
        "warn" => "warn",
        "debug" => "debug",
        "trace" => "trace",
        "silent" => "off",
        _ => "info",
    };
    // sqlx logs every statement at debug; keep that behind LOG_LEVEL=trace.
    let sqlx = if level == "trace" { "trace" } else { "warn" };
    EnvFilter::new(format!("{level},sqlx={sqlx}"))
}

/// Logs can hold the setup token and client addresses: a new file is readable
/// by owner and group only, whatever the umask (GH#443). An existing file keeps its mode.
fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o640);
    options.open(path)
}

pub fn init(level: &str, log_file: Option<&Path>) -> anyhow::Result<()> {
    let builder = tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter(level))
        .with_current_span(true)
        .with_span_list(false)
        .with_target(false);
    match log_file {
        Some(path) => {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)?;
            }
            let file = open_append(path)?;
            builder.with_ansi(false).with_writer(Mutex::new(file)).init();
        }
        None => builder.with_writer(std::io::stdout).init(),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn the_log_file_is_created_owner_and_group_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("shadoucmdb-log-{}.jsonl", uuid::Uuid::new_v4()));
        drop(super::open_append(&path).unwrap());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        std::fs::remove_file(&path).unwrap();
        assert_eq!(mode & !0o640, 0, "mode {mode:o} is 0640 or stricter");
    }
}
