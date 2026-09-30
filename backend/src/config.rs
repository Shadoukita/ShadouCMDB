//! Runtime configuration, read only from environment variables.
//!
//! The variables and their semantics are the ones documented in `.env.example`.
//! There are no host/port/user defaults for the database: PostgreSQL is an
//! external service and the operator must say where it lives.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::Context;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SslMode {
    /// Plain TCP.
    Disable,
    /// Encrypted, server certificate not verified. Explicit opt-in only.
    Require,
    /// Encrypted, certificate chain and hostname verified. The default.
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

/// `Debug` shows `url` and `password` only as set or unset: a connection
/// string carries the password, and a stray `{:?}` must not log it.
#[derive(Clone)]
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
    /// Set on the schema owner's connection only ([`Config::schema_owner_database`]).
    pub roles: RoleNames,
}

/// The API and maintenance database roles, whatever the operator named them:
/// the users of DATABASE_URL and MAINTENANCE_DATABASE_URL. The schema owner's
/// sessions carry them as `shadoucmdb.app_role` and `shadoucmdb.maintenance_role`
/// so migrations grant to, and hand ownership to, the right roles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleNames {
    pub app: Option<String>,
    pub maintenance: Option<String>,
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
    /// The address users open the web UI at (`PUBLIC_URL`), without a trailing
    /// slash. OIDC sign-in builds its redirect URI from it, never from the
    /// request's Host header; unset, OIDC sign-in is unavailable.
    pub public_url: Option<String>,
    /// `OIDC_ALLOWED_HOSTS`: the only hosts OIDC discovery, key and token
    /// requests may go to; None (unset) allows any host.
    pub oidc_allowed_hosts: Option<crate::auth::sso::oidc::AllowedHosts>,
    /// `SETUP_TOKEN`: the first-run setup token chosen by the operator; None
    /// generates one (see `auth::setup_token`).
    pub setup_token: Option<crate::auth::secret::Secret>,
    /// `SETUP_TOKEN_FILE`: where a generated setup token is written. `main`
    /// defaults it to `setup-token` next to the env file.
    pub setup_token_file: Option<PathBuf>,
    /// `TRUSTED_PROXIES`: whose forwarding headers the sign-in throttle reads
    /// (see [`crate::auth::session::throttle_ip`]).
    pub trusted_proxies: crate::auth::session::TrustedProxies,
    /// `SIGN_IN_FAILURE_FLOOR_MS`: the least time a refused sign-in (401) takes,
    /// so its answer does not tell which names are local accounts (GH#216).
    pub sign_in_failure_floor: Duration,
}

/// Who may read `/openapi.json` and the Swagger UI at `/docs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiDocs {
    /// Not served (404).
    Off,
    /// Any signed-in user (the browser's session cookie).
    Authenticated,
    /// Anyone who can reach the server.
    Public,
}

impl ApiDocs {
    pub fn as_str(self) -> &'static str {
        match self {
            ApiDocs::Off => "off",
            ApiDocs::Authenticated => "authenticated",
            ApiDocs::Public => "public",
        }
    }
}

/// Transport timeouts; they bound how long one client can hold a connection
/// or a request task (slowloris, stalled uploads).
#[derive(Debug, Clone)]
pub struct HttpConfig {
    /// Time allowed to send the request line and headers (HTTP/1).
    pub header_read_timeout: Duration,
    /// Time allowed for a whole request, body upload included, until the response starts.
    pub request_timeout: Duration,
    /// Requests handled at once; more are answered 503 SERVER_BUSY (bounds buffered bodies).
    pub max_concurrent_requests: usize,
}

/// Bulk import limits (SHAA-714 §3.5). They protect the host, so they are
/// operator settings; the instance switch itself lives in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportConfig {
    /// `IMPORT_ALLOWED=false` keeps bulk import off whatever an administrator sets.
    pub allowed: bool,
    /// Largest uploaded file (`IMPORT_MAX_FILE_MB`).
    pub max_file_bytes: u64,
    /// Most data rows per file (`IMPORT_MAX_ROWS`).
    pub max_rows: u32,
    /// Uploaded bytes stored at once across all jobs (`IMPORT_MAX_STORED_MB`).
    pub max_stored_bytes: u64,
    /// Time allowed for one whole upload (`IMPORT_UPLOAD_TIMEOUT_SECS`); it
    /// replaces `HTTP_REQUEST_TIMEOUT_SECS` on the upload route.
    pub upload_timeout: Duration,
    /// Import workers per server process (`IMPORT_WORKERS`).
    pub workers: usize,
}

const MIB: u64 = 1024 * 1024;

impl Default for ImportConfig {
    fn default() -> Self {
        ImportConfig {
            allowed: true,
            max_file_bytes: 50 * MIB,
            max_rows: 100_000,
            max_stored_bytes: 2048 * MIB,
            upload_timeout: Duration::from_secs(900),
            workers: 1,
        }
    }
}

/// Where exported audit events go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditSink {
    Stdout,
    /// Appended to, one event per line.
    File(PathBuf),
    /// Syslog over UDP (RFC 5426): one event per datagram.
    Udp(String),
    /// Syslog over TCP (RFC 6587 octet counting).
    Tcp(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditFormat {
    /// One JSON object per event.
    Json,
    /// RFC 5424 syslog message, the event as JSON in MSG.
    Rfc5424,
}

#[derive(Debug, Clone)]
pub struct AuditExportConfig {
    pub sink: AuditSink,
    pub format: AuditFormat,
    /// Syslog facility (0-23) for RFC 5424; 13 is "log audit".
    pub facility: u8,
    pub poll_interval: Duration,
}

#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// Record the client IP (sessions and sign-in events).
    pub capture_client_ip: bool,
    /// Record the browser's User-Agent (sessions and sign-in events).
    pub capture_user_agent: bool,
    /// `None`: events stay in the database only.
    pub export: Option<AuditExportConfig>,
}

impl Default for AuditConfig {
    fn default() -> Self {
        AuditConfig { capture_client_ip: true, capture_user_agent: true, export: None }
    }
}

/// The key that encrypts secrets the server reads back (TOTP seeds), kept
/// outside the database; see [`crate::secrets`]. Only paths here: the files
/// are read by the commands that need the key.
#[derive(Debug, Clone, Default)]
pub struct EncryptionConfig {
    /// `ENCRYPTION_KEY_FILE`: required by `serve`.
    pub key_file: Option<PathBuf>,
    /// `ENCRYPTION_KEY_PREVIOUS_FILE`: set only while rotating to a new key.
    pub previous_key_file: Option<PathBuf>,
}

/// Bounds of one impact analysis (`IMPACT_*`); see [`crate::modules::impact`].
/// Each has a compile-time ceiling, so a misconfigured variable cannot unbound
/// the traversal. The concurrency caps count per process (per replica).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImpactConfig {
    /// Largest `depth` a request may ask for.
    pub max_depth: i32,
    /// Largest `maxNodes` a request may ask for.
    pub max_nodes: i32,
    /// Wall-clock deadline of the traversal.
    pub timeout: Duration,
    /// Analyses running at once in this process; more are answered 503 SERVER_BUSY.
    pub max_concurrent: usize,
    /// Analyses one user (or their API tokens) runs at once; more are answered 429 RATE_LIMITED.
    pub max_concurrent_per_user: usize,
}

impl Default for ImpactConfig {
    fn default() -> Self {
        ImpactConfig {
            max_depth: 10,
            max_nodes: 2000,
            timeout: Duration::from_millis(5000),
            max_concurrent: 8,
            max_concurrent_per_user: 2,
        }
    }
}

/// Hard ceilings of the `IMPACT_*` settings.
pub const IMPACT_MAX_DEPTH_CEILING: i32 = 20;
pub const IMPACT_MAX_NODES_CEILING: i32 = 10_000;
pub const IMPACT_TIMEOUT_MS_CEILING: u64 = 30_000;

/// Well below `HTTP_REQUEST_TIMEOUT_SECS`, which also bounds the refused sign-in.
const MAX_SIGN_IN_FAILURE_FLOOR_MS: u64 = 10_000;
const DEFAULT_SESSION_IDLE_MINUTES: u64 = 12 * 60;
const DEFAULT_SESSION_MAX_AGE_HOURS: u64 = 7 * 24;

/// `Debug` redacts the connection strings (see [`DatabaseConfig`]).
#[derive(Clone)]
pub struct Config {
    pub api_host: String,
    pub api_port: u16,
    pub cors_origins: Vec<String>,
    /// Where browsers send CSP violation reports; `None` sends none.
    pub csp_report_uri: Option<String>,
    pub api_docs: ApiDocs,
    pub http: HttpConfig,
    pub database: DatabaseConfig,
    /// `shadoucmdb migrate`, `restore`, `factory-reset` and `decommission` connect with
    /// this instead of `database` (the schema owner role).
    pub migration_url: Option<String>,
    /// `shadoucmdb prune-audit` connects only with this (the maintenance role).
    pub maintenance_url: Option<String>,
    pub auth: AuthConfig,
    pub audit: AuditConfig,
    pub encryption: EncryptionConfig,
    pub impact: ImpactConfig,
    pub imports: ImportConfig,
}

/// The env file the variables were read from (`--env-file`, or the `.env` found).
static ENV_FILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Records the env file `main` loaded: a generated setup token is written next to it.
pub fn set_env_file(path: PathBuf) {
    let _ = ENV_FILE.set(path);
}

/// A secret as it appears in `Debug` output: whether it is set, never its value.
fn redacted(secret: &Option<String>) -> Option<&'static str> {
    secret.as_ref().map(|_| "<redacted>")
}

impl std::fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Destructured so a new field has to be placed here, redacted or not.
        let DatabaseConfig {
            url,
            host,
            port,
            database,
            user,
            password,
            ssl,
            ssl_ca_file,
            pool_max,
            statement_timeout,
            connect_timeout,
            roles,
        } = self;
        f.debug_struct("DatabaseConfig")
            .field("url", &redacted(url))
            .field("host", host)
            .field("port", port)
            .field("database", database)
            .field("user", user)
            .field("password", &redacted(password))
            .field("ssl", ssl)
            .field("ssl_ca_file", ssl_ca_file)
            .field("pool_max", pool_max)
            .field("statement_timeout", statement_timeout)
            .field("connect_timeout", connect_timeout)
            .field("roles", roles)
            .finish()
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Config {
            api_host,
            api_port,
            cors_origins,
            csp_report_uri,
            api_docs,
            http,
            database,
            migration_url,
            maintenance_url,
            auth,
            audit,
            encryption,
            impact,
            imports,
        } = self;
        f.debug_struct("Config")
            .field("api_host", api_host)
            .field("api_port", api_port)
            .field("cors_origins", cors_origins)
            .field("csp_report_uri", csp_report_uri)
            .field("api_docs", api_docs)
            .field("http", http)
            .field("database", database)
            .field("migration_url", &redacted(migration_url))
            .field("maintenance_url", &redacted(maintenance_url))
            .field("auth", auth)
            .field("audit", audit)
            .field("encryption", encryption)
            .field("impact", impact)
            .field("imports", imports)
            .finish()
    }
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

    fn bool(&mut self, key: &str, default: bool) -> bool {
        self.one_of(key, &["true", "false"], if default { "true" } else { "false" }) == "true"
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

/// Validates `PUBLIC_URL`: an absolute http(s) URL with no credentials, query
/// or fragment. A path is allowed (a reverse proxy serving the UI below one).
fn parse_public_url(raw: &str) -> Result<String, String> {
    const EXPECTED: &str = "expected the address users open the web UI at, e.g. https://cmdb.example.com";
    let url = url::Url::parse(raw.trim()).map_err(|_| format!("\"{raw}\" is not a URL; {EXPECTED}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(format!("\"{raw}\" is not an http(s) URL; {EXPECTED}"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not contain a user name or password".to_owned());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(format!("\"{raw}\" must not have a query or fragment; {EXPECTED}"));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
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

/// Parses `AUDIT_EXPORT`: `stdout`, `file:<path>`, `udp://host:port` or `tcp://host:port`.
fn parse_audit_sink(raw: &str) -> Result<AuditSink, String> {
    const EXPECTED: &str = "expected off, stdout, file:/path/to/audit.log, udp://host:port or tcp://host:port";
    if raw == "stdout" {
        return Ok(AuditSink::Stdout);
    }
    if let Some(path) = raw.strip_prefix("file:") {
        return if path.is_empty() {
            Err(format!("file: needs a path; {EXPECTED}"))
        } else {
            Ok(AuditSink::File(path.into()))
        };
    }
    let (scheme, addr) = raw.split_once("://").ok_or_else(|| format!("\"{raw}\" is not recognised; {EXPECTED}"))?;
    let valid = addr.rsplit_once(':').is_some_and(|(host, port)| {
        !host.is_empty() && !addr.contains('/') && port.parse::<u16>().is_ok_and(|p| p > 0)
    });
    if !valid {
        return Err(format!("\"{raw}\" needs a host and a port, e.g. {scheme}://siem.example.com:514"));
    }
    match scheme {
        "udp" => Ok(AuditSink::Udp(addr.to_owned())),
        "tcp" => Ok(AuditSink::Tcp(addr.to_owned())),
        _ => Err(format!("scheme \"{scheme}://\" is not supported; {EXPECTED}")),
    }
}

impl Config {
    /// The connection for commands that change the schema: MIGRATION_DATABASE_URL
    /// when set, else the API's. It carries the API and maintenance role names.
    pub fn schema_owner_database(self) -> anyhow::Result<DatabaseConfig> {
        let roles = RoleNames {
            app: Some(crate::db::user_name(&self.database).context("DATABASE_URL")?),
            maintenance: match &self.maintenance_url {
                Some(url) => {
                    Some(crate::db::user_name(&self.database.with_url(url)).context("MAINTENANCE_DATABASE_URL")?)
                }
                None => None,
            },
        };
        let db = match &self.migration_url {
            Some(url) => self.database.with_url(url),
            None => self.database,
        };
        Ok(DatabaseConfig { roles, ..db })
    }

    pub fn from_env() -> anyhow::Result<Config> {
        let mut cfg = Config::from_lookup(&|key| std::env::var(key).ok())?;
        if cfg.auth.setup_token_file.is_none() {
            cfg.auth.setup_token_file = ENV_FILE.get().and_then(|p| p.parent()).map(|dir| dir.join("setup-token"));
        }
        Ok(cfg)
    }

    /// `from_env` with the environment supplied by the caller, so tests need not mutate the process environment.
    fn from_lookup(env: &dyn Fn(&str) -> Option<String>) -> anyhow::Result<Config> {
        let mut r = Reader { env, errors: Vec::new() };

        // LOG_LEVEL is applied by logging::init before the config is loaded.
        r.one_of("LOG_LEVEL", &["fatal", "error", "warn", "info", "debug", "trace", "silent"], "info");
        let api_host = r.string("API_HOST", "0.0.0.0");
        let api_port = r.int::<u16>("API_PORT", 1, 65535).unwrap_or(3000);
        let api_docs = match r.one_of("API_DOCS", &["off", "authenticated", "public"], "off").as_str() {
            "authenticated" => ApiDocs::Authenticated,
            "public" => ApiDocs::Public,
            _ => ApiDocs::Off,
        };
        let header_read_timeout_secs = r.int::<u64>("HTTP_HEADER_READ_TIMEOUT_SECS", 1, 3600).unwrap_or(10);
        let request_timeout_secs = r.int::<u64>("HTTP_REQUEST_TIMEOUT_SECS", 1, 86_400).unwrap_or(120);
        let max_concurrent_requests = r.int::<usize>("HTTP_MAX_CONCURRENT_REQUESTS", 1, 1_000_000).unwrap_or(512);

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
        let ssl = match r.one_of("DATABASE_SSL", &["disable", "require", "verify-full"], "verify-full").as_str() {
            "disable" => SslMode::Disable,
            "require" => SslMode::Require,
            _ => SslMode::VerifyFull,
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

        let capture_client_ip = r.bool("AUDIT_CAPTURE_CLIENT_IP", true);
        let capture_user_agent = r.bool("AUDIT_CAPTURE_USER_AGENT", true);
        let sink = match r.string("AUDIT_EXPORT", "off").as_str() {
            "off" => None,
            raw => parse_audit_sink(raw).map_err(|e| r.errors.push(format!("AUDIT_EXPORT: {e}"))).ok(),
        };
        let network_sink = matches!(sink, Some(AuditSink::Udp(_) | AuditSink::Tcp(_)));
        let format = match r
            .one_of("AUDIT_EXPORT_FORMAT", &["json", "rfc5424"], if network_sink { "rfc5424" } else { "json" })
            .as_str()
        {
            "rfc5424" => AuditFormat::Rfc5424,
            _ => AuditFormat::Json,
        };
        let facility = r.int::<u8>("AUDIT_SYSLOG_FACILITY", 0, 23).unwrap_or(13);
        let poll_ms = r.int::<u64>("AUDIT_EXPORT_POLL_MS", 100, 3_600_000).unwrap_or(2_000);
        let export = sink.map(|sink| AuditExportConfig {
            sink,
            format,
            facility,
            poll_interval: Duration::from_millis(poll_ms),
        });
        let public_url = r
            .raw("PUBLIC_URL")
            .and_then(|s| parse_public_url(&s).map_err(|e| r.errors.push(format!("PUBLIC_URL: {e}"))).ok());
        let oidc_allowed_hosts = r.raw("OIDC_ALLOWED_HOSTS").and_then(|s| {
            crate::auth::sso::oidc::AllowedHosts::parse(&s)
                .map_err(|e| r.errors.push(format!("OIDC_ALLOWED_HOSTS: {e}")))
                .ok()
        });
        let setup_token = r.raw("SETUP_TOKEN").and_then(|token| {
            let length = token.chars().count();
            if length < crate::auth::setup_token::MIN_PRESET_LENGTH {
                r.errors.push(format!(
                    "SETUP_TOKEN: must be at least {} characters, got {length}; generate one with e.g. \
                     `openssl rand -hex 32`, or leave it unset and the server generates one",
                    crate::auth::setup_token::MIN_PRESET_LENGTH
                ));
                None
            } else {
                Some(crate::auth::secret::Secret::from(token))
            }
        });
        let setup_token_file = r.raw("SETUP_TOKEN_FILE").map(PathBuf::from);
        let trusted_proxies = r
            .raw("TRUSTED_PROXIES")
            .and_then(|s| {
                crate::auth::session::TrustedProxies::parse(&s)
                    .map_err(|e| r.errors.push(format!("TRUSTED_PROXIES: {e}")))
                    .ok()
            })
            .unwrap_or_default();
        let sign_in_failure_floor_ms =
            r.int::<u64>("SIGN_IN_FAILURE_FLOOR_MS", 0, MAX_SIGN_IN_FAILURE_FLOOR_MS).unwrap_or(1_000);
        if sign_in_failure_floor_ms >= request_timeout_secs.saturating_mul(1_000) {
            r.errors.push(format!(
                "SIGN_IN_FAILURE_FLOOR_MS: {sign_in_failure_floor_ms} ms is not below HTTP_REQUEST_TIMEOUT_SECS \
                 ({request_timeout_secs} s), so refused sign-ins would time out instead of answering 401"
            ));
        }

        let impact_defaults = ImpactConfig::default();
        let impact_timeout_ms = r
            .int::<u64>("IMPACT_TIMEOUT_MS", 1, IMPACT_TIMEOUT_MS_CEILING)
            .unwrap_or(impact_defaults.timeout.as_millis() as u64);
        if impact_timeout_ms >= request_timeout_secs.saturating_mul(1_000) {
            r.errors.push(format!(
                "IMPACT_TIMEOUT_MS: {impact_timeout_ms} ms is not below HTTP_REQUEST_TIMEOUT_SECS \
                 ({request_timeout_secs} s), so a long impact analysis would time out instead of answering a \
                 truncated result"
            ));
        }
        // Each running analysis holds a pool connection: at most half the pool,
        // so the rest of the API (sign-in, /readyz, edits) keeps connections.
        let impact_concurrent_limit = (pool_max as usize / 2).max(1);
        let impact_max_concurrent = r
            .int::<usize>("IMPACT_MAX_CONCURRENT", 1, 1_000)
            .unwrap_or(impact_defaults.max_concurrent.min(impact_concurrent_limit));
        if impact_max_concurrent > impact_concurrent_limit {
            r.errors.push(format!(
                "IMPACT_MAX_CONCURRENT: {impact_max_concurrent} is above half of DATABASE_POOL_MAX ({pool_max}), \
                 so impact analyses could hold the connections the rest of the API needs; set it to at most \
                 {impact_concurrent_limit} or raise DATABASE_POOL_MAX"
            ));
        }
        let impact = ImpactConfig {
            max_depth: r
                .int::<i32>("IMPACT_MAX_DEPTH", 1, IMPACT_MAX_DEPTH_CEILING)
                .unwrap_or(impact_defaults.max_depth),
            max_nodes: r
                .int::<i32>("IMPACT_MAX_NODES", 1, IMPACT_MAX_NODES_CEILING)
                .unwrap_or(impact_defaults.max_nodes),
            timeout: Duration::from_millis(impact_timeout_ms),
            max_concurrent: impact_max_concurrent,
            max_concurrent_per_user: r
                .int::<usize>("IMPACT_MAX_CONCURRENT_PER_USER", 1, 1_000)
                .unwrap_or(impact_defaults.max_concurrent_per_user),
        };

        let encryption = EncryptionConfig {
            key_file: r.raw("ENCRYPTION_KEY_FILE").map(PathBuf::from),
            previous_key_file: r.raw("ENCRYPTION_KEY_PREVIOUS_FILE").map(PathBuf::from),
        };

        let defaults = ImportConfig::default();
        let imports = ImportConfig {
            allowed: r.bool("IMPORT_ALLOWED", true),
            max_file_bytes: r.int::<u64>("IMPORT_MAX_FILE_MB", 1, 200).map_or(defaults.max_file_bytes, |mb| mb * MIB),
            max_rows: r.int::<u32>("IMPORT_MAX_ROWS", 1, 1_000_000).unwrap_or(defaults.max_rows),
            max_stored_bytes: r
                .int::<u64>("IMPORT_MAX_STORED_MB", 100, 100_000)
                .map_or(defaults.max_stored_bytes, |mb| mb * MIB),
            upload_timeout: r
                .int::<u64>("IMPORT_UPLOAD_TIMEOUT_SECS", 60, 3600)
                .map_or(defaults.upload_timeout, Duration::from_secs),
            workers: r.int::<usize>("IMPORT_WORKERS", 1, 4).unwrap_or(defaults.workers),
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
            api_docs,
            http: HttpConfig {
                header_read_timeout: Duration::from_secs(header_read_timeout_secs),
                request_timeout: Duration::from_secs(request_timeout_secs),
                max_concurrent_requests,
            },
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
                roles: RoleNames::default(),
            },
            migration_url,
            maintenance_url,
            auth: AuthConfig {
                session_idle: Duration::from_secs(session_idle_minutes * 60),
                session_max_age: Duration::from_secs(session_max_age_hours * 3600),
                cookie_secure,
                public_url,
                oidc_allowed_hosts,
                setup_token,
                setup_token_file,
                trusted_proxies,
                sign_in_failure_floor: Duration::from_millis(sign_in_failure_floor_ms),
            },
            audit: AuditConfig { capture_client_ip, capture_user_agent, export },
            encryption,
            impact,
            imports,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_url_is_an_absolute_address_without_trailing_slash() {
        assert_eq!(parse_public_url("https://cmdb.example.com/").unwrap(), "https://cmdb.example.com");
        assert_eq!(parse_public_url("https://example.com/cmdb/").unwrap(), "https://example.com/cmdb");
        assert_eq!(parse_public_url("http://10.0.0.5:8080").unwrap(), "http://10.0.0.5:8080");
        for bad in ["cmdb.example.com", "/cmdb", "ftp://x", "https://u:p@x", "https://x/?a=1", "https://x/#f"] {
            assert!(parse_public_url(bad).is_err(), "{bad}");
        }
    }

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
        load_with(&[("CSP_REPORT_URI", csp_report_uri)])
    }

    fn load_with(vars: &[(&str, &str)]) -> anyhow::Result<Config> {
        Config::from_lookup(&|key| match key {
            "DATABASE_URL" => Some("postgres://cmdb@db/cmdb".into()),
            _ => vars.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_owned()),
        })
    }

    #[test]
    fn debug_output_redacts_database_secrets() {
        let cfg = Config::from_lookup(&|key| match key {
            "DATABASE_URL" => Some("postgres://cmdb:url-secret@db/cmdb".into()),
            "MIGRATION_DATABASE_URL" => Some("postgres://owner:migration-secret@db/cmdb".into()),
            "MAINTENANCE_DATABASE_URL" => Some("postgres://maint:maintenance-secret@db/cmdb".into()),
            "PGPASSWORD" => Some("pg-secret".into()),
            _ => None,
        })
        .unwrap();
        let shown = format!("{cfg:?} {:#?}", cfg.database);
        // The failure message names only the fixture, never the Debug output that would carry it.
        for fixture in ["url-secret", "migration-secret", "maintenance-secret", "pg-secret"] {
            assert!(!shown.contains(fixture), "Debug output leaks the {fixture} fixture");
        }
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn hardened_defaults() {
        let cfg = load_with(&[]).unwrap();
        assert_eq!(cfg.api_docs, ApiDocs::Off);
        assert_eq!(cfg.http.header_read_timeout, Duration::from_secs(10));
        assert_eq!(cfg.http.request_timeout, Duration::from_secs(120));
        assert_eq!(cfg.http.max_concurrent_requests, 512);
        assert!(cfg.audit.capture_client_ip && cfg.audit.capture_user_agent);
        assert!(cfg.audit.export.is_none());
    }

    #[test]
    fn api_docs_switch() {
        assert_eq!(load_with(&[("API_DOCS", "public")]).unwrap().api_docs, ApiDocs::Public);
        assert_eq!(load_with(&[("API_DOCS", "authenticated")]).unwrap().api_docs, ApiDocs::Authenticated);
        assert!(load_with(&[("API_DOCS", "yes")]).unwrap_err().to_string().contains("API_DOCS"));
    }

    #[test]
    fn oidc_allowed_hosts() {
        assert_eq!(load_with(&[]).unwrap().auth.oidc_allowed_hosts, None, "unset: any host");
        let cfg = load_with(&[("OIDC_ALLOWED_HOSTS", "login.example.com, idp.corp.example:8443")]).unwrap();
        let allowed = cfg.auth.oidc_allowed_hosts.unwrap();
        assert!(allowed.allows(&url::Url::parse("https://login.example.com/x").unwrap()));
        assert!(!allowed.allows(&url::Url::parse("https://idp.corp.example/x").unwrap()));
        let err = load_with(&[("OIDC_ALLOWED_HOSTS", "*.example.com")]).unwrap_err().to_string();
        assert!(err.contains("OIDC_ALLOWED_HOSTS"), "{err}");
    }

    #[test]
    fn trusted_proxies_and_the_sign_in_failure_floor() {
        let cfg = load_with(&[]).unwrap();
        assert!(cfg.auth.trusted_proxies.is_empty(), "unset: no proxy is trusted");
        assert_eq!(cfg.auth.sign_in_failure_floor, Duration::from_secs(1));
        let cfg =
            load_with(&[("TRUSTED_PROXIES", "10.0.0.0/8, 192.0.2.7"), ("SIGN_IN_FAILURE_FLOOR_MS", "0")]).unwrap();
        assert!(cfg.auth.trusted_proxies.contains("10.1.2.3".parse().unwrap()));
        assert_eq!(cfg.auth.sign_in_failure_floor, Duration::ZERO);
        for (key, bad) in [
            ("TRUSTED_PROXIES", "0.0.0.0/0"),
            ("TRUSTED_PROXIES", "lb.example.com"),
            ("SIGN_IN_FAILURE_FLOOR_MS", "60000"),
        ] {
            let err = load_with(&[(key, bad)]).unwrap_err().to_string();
            assert!(err.contains(key), "{err}");
        }
        let err = load_with(&[("SIGN_IN_FAILURE_FLOOR_MS", "2000"), ("HTTP_REQUEST_TIMEOUT_SECS", "2")]).unwrap_err();
        assert!(err.to_string().contains("SIGN_IN_FAILURE_FLOOR_MS"), "{err}");
    }

    #[test]
    fn impact_limits_stay_within_the_pool_and_the_request_timeout() {
        let cfg = load_with(&[]).unwrap();
        assert_eq!(cfg.impact.max_concurrent, 5, "default: min(8, DATABASE_POOL_MAX 10 / 2)");
        assert_eq!(load_with(&[("DATABASE_POOL_MAX", "40")]).unwrap().impact.max_concurrent, 8);
        assert_eq!(load_with(&[("DATABASE_POOL_MAX", "1")]).unwrap().impact.max_concurrent, 1);
        let ok = load_with(&[("DATABASE_POOL_MAX", "30"), ("IMPACT_MAX_CONCURRENT", "15")]).unwrap();
        assert_eq!(ok.impact.max_concurrent, 15);
        for vars in [
            &[("DATABASE_POOL_MAX", "10"), ("IMPACT_MAX_CONCURRENT", "10")][..],
            &[("DATABASE_POOL_MAX", "10"), ("IMPACT_MAX_CONCURRENT", "6")][..],
        ] {
            let err = load_with(vars).unwrap_err().to_string();
            assert!(err.contains("IMPACT_MAX_CONCURRENT") && err.contains("DATABASE_POOL_MAX"), "{err}");
        }
        for (key, bad) in [("IMPACT_MAX_DEPTH", "21"), ("IMPACT_MAX_NODES", "0"), ("IMPACT_TIMEOUT_MS", "30001")] {
            let err = load_with(&[(key, bad)]).unwrap_err().to_string();
            assert!(err.contains(key), "{err}");
        }
        let err = load_with(&[("IMPACT_TIMEOUT_MS", "3000"), ("HTTP_REQUEST_TIMEOUT_SECS", "3")]).unwrap_err();
        assert!(err.to_string().contains("IMPACT_TIMEOUT_MS"), "{err}");
    }

    #[test]
    fn import_limits() {
        assert_eq!(load_with(&[]).unwrap().imports, ImportConfig::default());
        let cfg = load_with(&[
            ("IMPORT_ALLOWED", "false"),
            ("IMPORT_MAX_FILE_MB", "200"),
            ("IMPORT_MAX_ROWS", "1"),
            ("IMPORT_MAX_STORED_MB", "100"),
            ("IMPORT_UPLOAD_TIMEOUT_SECS", "60"),
            ("IMPORT_WORKERS", "4"),
        ])
        .unwrap()
        .imports;
        assert_eq!(
            cfg,
            ImportConfig {
                allowed: false,
                max_file_bytes: 200 * MIB,
                max_rows: 1,
                max_stored_bytes: 100 * MIB,
                upload_timeout: Duration::from_secs(60),
                workers: 4,
            }
        );
        for (key, bad) in [
            ("IMPORT_ALLOWED", "maybe"),
            ("IMPORT_MAX_FILE_MB", "201"),
            ("IMPORT_MAX_FILE_MB", "0"),
            ("IMPORT_MAX_ROWS", "1000001"),
            ("IMPORT_MAX_STORED_MB", "99"),
            ("IMPORT_UPLOAD_TIMEOUT_SECS", "3601"),
            ("IMPORT_WORKERS", "5"),
        ] {
            let err = load_with(&[(key, bad)]).unwrap_err().to_string();
            assert!(err.contains(key), "{key}={bad}: {err}");
        }
    }

    #[test]
    fn audit_capture_switches() {
        let cfg = load_with(&[("AUDIT_CAPTURE_CLIENT_IP", "false"), ("AUDIT_CAPTURE_USER_AGENT", "false")]).unwrap();
        assert!(!cfg.audit.capture_client_ip && !cfg.audit.capture_user_agent);
        assert!(load_with(&[("AUDIT_CAPTURE_CLIENT_IP", "no")]).is_err());
    }

    #[test]
    fn audit_export_targets() {
        let export = |v: &str| load_with(&[("AUDIT_EXPORT", v)]).map(|c| c.audit.export);
        assert!(export("off").unwrap().is_none());
        let e = export("udp://siem.example.com:514").unwrap().unwrap();
        assert_eq!(e.sink, AuditSink::Udp("siem.example.com:514".into()));
        assert_eq!(e.format, AuditFormat::Rfc5424, "syslog transports default to RFC 5424");
        assert_eq!(e.facility, 13);
        let e = export("tcp://[2001:db8::1]:6514").unwrap().unwrap();
        assert_eq!(e.sink, AuditSink::Tcp("[2001:db8::1]:6514".into()));
        let e = export("file:/var/log/shadoucmdb/audit.jsonl").unwrap().unwrap();
        assert_eq!(e.sink, AuditSink::File("/var/log/shadoucmdb/audit.jsonl".into()));
        assert_eq!(e.format, AuditFormat::Json);
        assert_eq!(export("stdout").unwrap().unwrap().sink, AuditSink::Stdout);
        for bad in ["syslog", "udp://siem.example.com", "http://siem:514", "tcp://:514", "udp://h:0", "file:"] {
            assert!(export(bad).unwrap_err().to_string().contains("AUDIT_EXPORT: "), "{bad}");
        }
        let cfg = load_with(&[("AUDIT_EXPORT", "stdout"), ("AUDIT_EXPORT_FORMAT", "rfc5424")]).unwrap();
        assert_eq!(cfg.audit.export.unwrap().format, AuditFormat::Rfc5424);
    }

    #[test]
    fn database_ssl_defaults_to_verify_full() {
        assert_eq!(load("").unwrap().database.ssl, SslMode::VerifyFull);
        for (value, mode) in
            [("verify-full", SslMode::VerifyFull), ("require", SslMode::Require), ("disable", SslMode::Disable)]
        {
            let cfg = Config::from_lookup(&|key| match key {
                "DATABASE_URL" => Some("postgres://cmdb@db/cmdb".into()),
                "DATABASE_SSL" => Some(value.into()),
                _ => None,
            })
            .unwrap();
            assert_eq!(cfg.database.ssl, mode, "{value}");
        }
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
