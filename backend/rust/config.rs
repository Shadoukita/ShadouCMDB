//! Runtime configuration, read only from environment variables.
//!
//! The variables and their semantics are the ones documented in `.env.example`
//! (shared with the Node API). There are no host/port/user defaults for the
//! database: PostgreSQL is an external service and the operator must say where
//! it lives.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslMode {
    /// Plain TCP.
    Disable,
    /// Encrypted, server certificate not verified.
    Require,
    /// Encrypted, certificate chain and hostname verified.
    VerifyFull,
}

impl SslMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SslMode::Disable => "disable",
            SslMode::Require => "require",
            SslMode::VerifyFull => "verify-full",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// Full connection string; takes precedence over the discrete PG* values.
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: u16,
    pub database: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub ssl: SslMode,
    pub ssl_ca_file: Option<PathBuf>,
    pub pool_max: u32,
    /// Zero disables the per-statement timeout.
    pub statement_timeout: Duration,
    pub connect_timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub api_host: String,
    pub api_port: u16,
    pub cors_origins: Vec<String>,
    pub database: DatabaseConfig,
}

/// Collects every problem so the operator sees them all at once.
struct Reader {
    errors: Vec<String>,
}

impl Reader {
    /// Empty strings (common in .env files and compose) count as unset.
    fn raw(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.is_empty())
    }

    fn string(&self, key: &str, default: &str) -> String {
        self.raw(key).unwrap_or_else(|| default.to_owned())
    }

    fn one_of(&mut self, key: &str, allowed: &[&str], default: &str) -> String {
        let value = self.string(key, default);
        if allowed.contains(&value.as_str()) {
            value
        } else {
            self.errors.push(format!("{key}: expected one of {}, got \"{value}\"", allowed.join(" | ")));
            default.to_owned()
        }
    }

    fn int<T>(&mut self, key: &str, min: T, max: T) -> Option<T>
    where
        T: FromStr + PartialOrd + Copy + std::fmt::Display,
    {
        let raw = self.raw(key)?;
        match raw.trim().parse::<T>() {
            Ok(v) if v >= min && v <= max => Some(v),
            _ => {
                self.errors.push(format!("{key}: expected an integer between {min} and {max}, got \"{raw}\""));
                None
            }
        }
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Config> {
        let mut r = Reader { errors: Vec::new() };

        // NODE_ENV is validated for compatibility with the Node API but changes nothing here;
        // LOG_LEVEL is applied by logging::init before the config is loaded.
        r.one_of("NODE_ENV", &["development", "production", "test"], "production");
        r.one_of("LOG_LEVEL", &["fatal", "error", "warn", "info", "debug", "trace", "silent"], "info");
        let api_host = r.string("API_HOST", "0.0.0.0");
        let api_port = r.int::<u16>("API_PORT", 1, 65535).unwrap_or(3000);

        let url = r.raw("DATABASE_URL");
        let host = r.raw("PGHOST");
        let port = r.int::<u16>("PGPORT", 1, 65535).unwrap_or(5432);
        let database = r.raw("PGDATABASE");
        let user = r.raw("PGUSER");
        // An empty password is a valid (if unwise) value, so read it verbatim.
        let password = std::env::var("PGPASSWORD").ok();
        if url.is_none() {
            for (key, value) in [("PGHOST", &host), ("PGDATABASE", &database), ("PGUSER", &user)] {
                if value.is_none() {
                    r.errors.push(format!("{key}: {key} is required when DATABASE_URL is not set"));
                }
            }
        }
        let ssl = match r.one_of("DATABASE_SSL", &["disable", "require", "verify-full"], "require").as_str() {
            "disable" => SslMode::Disable,
            "verify-full" => SslMode::VerifyFull,
            _ => SslMode::Require,
        };
        let ssl_ca_file = r.raw("DATABASE_SSL_CA_FILE").map(PathBuf::from);
        let pool_max = r.int::<u32>("DATABASE_POOL_MAX", 1, 200).unwrap_or(10);
        let statement_timeout_ms = r.int::<u64>("DATABASE_STATEMENT_TIMEOUT_MS", 0, u64::MAX).unwrap_or(30_000);
        let connect_timeout_ms = r.int::<u64>("DATABASE_CONNECT_TIMEOUT_MS", 100, u64::MAX).unwrap_or(5_000);

        let cors_origins = r
            .raw("CORS_ORIGINS")
            .map(|s| s.split(',').map(str::trim).filter(|o| !o.is_empty()).map(str::to_owned).collect())
            .unwrap_or_default();

        if !r.errors.is_empty() {
            let detail: Vec<String> = r.errors.iter().map(|e| format!("  - {e}")).collect();
            anyhow::bail!(
                "Invalid configuration:\n{}\nSee .env.example for every supported variable.",
                detail.join("\n")
            );
        }

        Ok(Config {
            api_host,
            api_port,
            cors_origins,
            database: DatabaseConfig {
                url,
                host,
                port,
                database,
                user,
                password,
                ssl,
                ssl_ca_file,
                pool_max,
                statement_timeout: Duration::from_millis(statement_timeout_ms),
                connect_timeout: Duration::from_millis(connect_timeout_ms),
            },
        })
    }
}
