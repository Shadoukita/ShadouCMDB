//! Runtime configuration, read only from environment variables.
//!
//! The variables and their semantics are the ones documented in `.env.example`.
//! There are no host/port/user defaults for the database: PostgreSQL is an
//! external service and the operator must say where it lives.

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

/// When session cookies get the `Secure` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieSecure {
    /// When the request arrived over HTTPS (X-Forwarded-Proto / Forwarded from the reverse proxy).
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// A session unused for this long ends.
    pub session_idle: Duration,
    /// A session ends this long after login, however active.
    pub session_max_age: Duration,
    pub cookie_secure: CookieSecure,
}

const DEFAULT_SESSION_IDLE_MINUTES: u64 = 12 * 60;
const DEFAULT_SESSION_MAX_AGE_HOURS: u64 = 7 * 24;

#[derive(Debug, Clone)]
pub struct Config {
    pub api_host: String,
    pub api_port: u16,
    pub cors_origins: Vec<String>,
    /// Where browsers send CSP violation reports; `None` sends none.
    pub csp_report_uri: Option<String>,
    pub database: DatabaseConfig,
    /// `shadoucmdb migrate` connects with this instead of `database` (the schema owner role).
    pub migration_url: Option<String>,
    /// `shadoucmdb prune-audit` connects only with this (the maintenance role).
    pub maintenance_url: Option<String>,
    pub auth: AuthConfig,
}

impl DatabaseConfig {
    /// The same TLS, pool and timeout settings for another role's connection string.
    pub fn with_url(&self, url: &str) -> DatabaseConfig {
        DatabaseConfig { url: Some(url.to_owned()), ..self.clone() }
    }
}

/// Collects every problem so the operator sees them all at once.
struct Reader<'a> {
    env: &'a dyn Fn(&str) -> Option<String>,
    errors: Vec<String>,
}

impl Reader<'_> {
    /// Empty strings (common in .env files and compose) count as unset.
    fn raw(&self, key: &str) -> Option<String> {
        (self.env)(key).filter(|v| !v.is_empty())
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

/// Parses the comma-separated `CORS_ORIGINS` list. Each entry must be exactly
/// what a browser sends in `Origin` (`scheme://host[:port]`, no path), because
/// the CORS layer compares them byte for byte: `*`, a trailing slash or an
/// upper-case host would otherwise be accepted here and silently match nothing.
/// (A literal `*` could not be honoured anyway: the session cookie makes these
/// credentialed requests, and browsers refuse `*` for those.)
fn parse_cors_origins(raw: &str) -> Result<Vec<String>, String> {
    let mut origins = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|o| !o.is_empty()) {
        // The URL parser accepts `*` in a host name, so wildcards are caught here.
        if entry.contains('*') {
            return Err(format!(
                "\"{entry}\" is not supported: wildcard origins match nothing. List each web UI origin \
                 explicitly, e.g. CORS_ORIGINS=https://cmdb.example.com,http://localhost:5173"
            ));
        }
        let origin = url::Url::parse(entry)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https") && u.username().is_empty() && u.password().is_none())
            .map(|u| u.origin().ascii_serialization());
        match origin {
            Some(o) if o == entry => origins.push(o),
            Some(o) => {
                return Err(format!("\"{entry}\" is not an origin as a browser sends it; did you mean \"{o}\"?"));
            }
            None => {
                return Err(format!(
                    "\"{entry}\" is not an origin; expected scheme://host[:port] with scheme http or https, \
                     e.g. https://cmdb.example.com"
                ));
            }
        }
    }
    Ok(origins)
}

/// Validates `CSP_REPORT_URI`. The value is pasted into the
/// `Content-Security-Policy` and `Reporting-Endpoints` headers, so anything
/// that could end the URL and start a new directive, header or header-list
/// entry is refused rather than escaped: `CSP_REPORT_URI=/r; script-src *`
/// must stop the server, not quietly weaken the policy.
fn parse_csp_report_uri(raw: &str) -> Result<String, String> {
    const EXPECTED: &str = "expected an absolute http(s) URL such as https://reports.example.com/csp \
                            or a path on this server such as /csp-reports";
    if let Some(c) = raw.chars().find(|c| !c.is_ascii_graphic()) {
        return Err(if c.is_ascii() {
            "contains whitespace or a control character".to_owned()
        } else {
            format!("contains the non-ASCII character {c:?}; percent-encode it")
        });
    }
    if let Some(c) = raw.chars().find(|c| matches!(c, ';' | ',' | '"' | '\\')) {
        return Err(format!("contains '{c}', which would split the header; percent-encode it"));
    }
    if raw.starts_with("//") {
        return Err(format!("\"{raw}\" is a scheme-relative URL; {EXPECTED}"));
    }
    if raw.starts_with('/') {
        return Ok(raw.to_owned());
    }
    let url = url::Url::parse(raw).map_err(|_| format!("\"{raw}\" is not a URL; {EXPECTED}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("scheme \"{}:\" is not allowed; {EXPECTED}", url.scheme()));
    }
    // Browsers receive this header; it must not carry credentials.
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not contain a user name or password".to_owned());
    }
    Ok(raw.to_owned())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Config> {
        Config::from_lookup(&|key| std::env::var(key).ok())
    }

    /// `from_env` with the environment supplied by the caller, so tests need not mutate the process environment.
    fn from_lookup(env: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<Config> {
        let mut r = Reader { env, errors: Vec::new() };

        // LOG_LEVEL is applied by logging::init before the config is loaded.
        r.one_of("LOG_LEVEL", &["fatal", "error", "warn", "info", "debug", "trace", "silent"], "info");
        let api_host = r.string("API_HOST", "0.0.0.0");
        let api_port = r.int::<u16>("API_PORT", 1, 65535).unwrap_or(3000);

        let url = r.raw("DATABASE_URL");
        let migration_url = r.raw("MIGRATION_DATABASE_URL");
        let maintenance_url = r.raw("MAINTENANCE_DATABASE_URL");
        let host = r.raw("PGHOST");
        let port = r.int::<u16>("PGPORT", 1, 65535).unwrap_or(5432);
        let database = r.raw("PGDATABASE");
        let user = r.raw("PGUSER");
        // An empty password is a valid (if unwise) value, so read it verbatim.
        let password = env("PGPASSWORD");
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

        let cors_origins = match r.raw("CORS_ORIGINS").map(|s| parse_cors_origins(&s)) {
            Some(Ok(origins)) => origins,
            Some(Err(e)) => {
                r.errors.push(format!("CORS_ORIGINS: {e}"));
                Vec::new()
            }
            None => Vec::new(),
        };
        let csp_report_uri = r
            .raw("CSP_REPORT_URI")
            .and_then(|s| parse_csp_report_uri(&s).map_err(|e| r.errors.push(format!("CSP_REPORT_URI: {e}"))).ok());

        let session_idle_minutes =
            r.int::<u64>("SESSION_IDLE_TIMEOUT_MINUTES", 5, 525_600).unwrap_or(DEFAULT_SESSION_IDLE_MINUTES);
        let session_max_age_hours =
            r.int::<u64>("SESSION_MAX_AGE_HOURS", 1, 8_760).unwrap_or(DEFAULT_SESSION_MAX_AGE_HOURS);
        let cookie_secure = match r.one_of("COOKIE_SECURE", &["auto", "always", "never"], "auto").as_str() {
            "always" => CookieSecure::Always,
            "never" => CookieSecure::Never,
            _ => CookieSecure::Auto,
        };

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
            csp_report_uri,
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
            migration_url,
            maintenance_url,
            auth: AuthConfig {
                session_idle: Duration::from_secs(session_idle_minutes * 60),
                session_max_age: Duration::from_secs(session_max_age_hours * 3600),
                cookie_secure,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cors_origins_accept_browser_origins() {
        assert_eq!(
            parse_cors_origins(" https://cmdb.example.com , http://localhost:5173,,http://10.0.0.5:8080").unwrap(),
            ["https://cmdb.example.com", "http://localhost:5173", "http://10.0.0.5:8080"]
        );
        assert_eq!(parse_cors_origins("").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn cors_origins_reject_wildcards_and_non_origins() {
        let err = parse_cors_origins("https://a.example.com,*").unwrap_err();
        assert!(err.contains("\"*\" is not supported"), "{err}");
        assert!(err.contains("explicitly"), "{err}");
        for bad in [
            "cmdb.example.com",
            "ftp://cmdb.example.com",
            "https://user@cmdb.example.com",
            "null",
            "https://*.example.com",
        ] {
            assert!(parse_cors_origins(bad).is_err(), "{bad} should be rejected");
        }
        // Would never match a browser's Origin header byte for byte; the error names the right spelling.
        assert!(
            parse_cors_origins("https://cmdb.example.com/")
                .unwrap_err()
                .contains("did you mean \"https://cmdb.example.com\"")
        );
        assert!(parse_cors_origins("https://CMDB.example.com").unwrap_err().contains("\"https://cmdb.example.com\""));
        assert!(
            parse_cors_origins("https://cmdb.example.com:443").unwrap_err().contains("\"https://cmdb.example.com\"")
        );
        assert!(parse_cors_origins("https://cmdb.example.com/app").is_err());
    }

    fn load(csp_report_uri: &str) -> anyhow::Result<Config> {
        Config::from_lookup(&|key| match key {
            "DATABASE_URL" => Some("postgres://cmdb@db/cmdb".into()),
            "CSP_REPORT_URI" => Some(csp_report_uri.into()),
            _ => None,
        })
    }

    #[test]
    fn csp_report_uri_accepts_http_urls_and_local_paths() {
        assert_eq!(load("").unwrap().csp_report_uri, None);
        for ok in ["https://reports.example.com/csp?app=cmdb", "http://10.0.0.5:8080/r", "/csp-reports"] {
            assert_eq!(load(ok).unwrap().csp_report_uri.as_deref(), Some(ok));
        }
    }

    #[test]
    fn csp_report_uri_rejects_anything_that_could_rewrite_the_policy() {
        for (bad, why) in [
            ("/r; script-src 'unsafe-inline' *", "whitespace"),
            ("/r;script-src", "';'"),
            ("/r,/s", "','"),
            ("/r s", "whitespace"),
            ("/r\nX-Injected: 1", "control character"),
            ("/r\"", "'\"'"),
            ("/café", "non-ASCII"),
            ("javascript:alert(1)", "scheme \"javascript:\""),
            ("data:text/plain,x", "','"),
            ("data:text/plain", "scheme \"data:\""),
            ("https://user:pw@reports.example.com/r", "user name or password"),
            ("https://user@reports.example.com/r", "user name or password"),
            ("//reports.example.com/r", "scheme-relative"),
            ("reports.example.com/r", "not a URL"),
        ] {
            let err = load(bad).unwrap_err().to_string();
            assert!(err.contains("CSP_REPORT_URI: "), "{bad}: {err}");
            assert!(err.contains(why), "{bad}: expected {why} in {err}");
        }
    }
}
