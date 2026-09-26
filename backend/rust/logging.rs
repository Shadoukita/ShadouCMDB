//! Structured logs: one JSON object per line (like the Node API's pino output),
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
            let file = OpenOptions::new().create(true).append(true).open(path)?;
            builder.with_ansi(false).with_writer(Mutex::new(file)).init();
        }
        None => builder.with_writer(std::io::stdout).init(),
    }
    Ok(())
}
