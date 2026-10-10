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
    /// Time an authenticated request's body may take to arrive (GH#556).
    pub body_timeout: Duration,
    /// Time a client may take none of a response before its connection is dropped (GH#682).
    pub send_timeout: Duration,
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
    /// Syslog over TLS (RFC 5425): certificate chain and host name verified.
    Tls(String),
}

impl AuditSink {
    /// The `host:port` of a network sink.
    pub fn address(&self) -> Option<&str> {
        match self {
            AuditSink::Udp(a) | AuditSink::Tcp(a) | AuditSink::Tls(a) => Some(a),
            AuditSink::Stdout | AuditSink::File(_) => None,
        }
    }
}

/// The host of `host:port`, without the brackets of an IPv6 literal.
pub fn sink_host(addr: &str) -> &str {
    let host = addr.rsplit_once(':').map_or(addr, |(host, _)| host);
    host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host)
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
    /// Extra trusted CA certificates (PEM) for `tls://`, added to the public
    /// and operating-system roots.
    pub tls_ca_file: Option<PathBuf>,
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
    /// Wall-clock deadline of the walk, from the start of the request; the
    /// result is then assembled within [`IMPACT_ASSEMBLY_ALLOWANCE_MS`].
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

/// Bounds of business services (`BUSINESS_SERVICE_*`, SHAA-927 §1.3); see
/// [`crate::modules::business_services`]. Each has a compile-time ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusinessServiceConfig {
    /// Direct members of one service, counted over the members the caller may view.
    pub max_members: i64,
    /// Longest chain of services including services.
    pub max_nesting: i32,
}

impl Default for BusinessServiceConfig {
    fn default() -> Self {
        BusinessServiceConfig { max_members: 5_000, max_nesting: 5 }
    }
}

/// Inventory CSV exports (`EXPORT_*`, GH#801); see
/// [`crate::modules::items::export`]. Each export holds a pool connection
/// while the client reads, so the cap is counted against `DATABASE_POOL_MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportConfig {
    /// Exports running at once in this process; more are answered 503 SERVER_BUSY.
    pub max_concurrent: usize,
}

impl ExportConfig {
    /// The default for a pool of `pool_max` connections: an eighth of it, at least one.
    pub fn for_pool(pool_max: u32) -> Self {
        ExportConfig { max_concurrent: (pool_max as usize / 8).max(1) }
    }
}

impl Default for ExportConfig {
    fn default() -> Self {
        ExportConfig::for_pool(10)
    }
}

/// Pool connections left to the rest of the API (sign-in, `/readyz`, edits)
/// when impact analyses, saved-view counts and exports all use their caps.
pub const POOL_RESERVE: usize = 2;

/// In-app notifications (`NOTIFICATION_*`, SHAA-2356); see
/// [`crate::modules::notifications`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationConfig {
    /// Days a notification is kept (`NOTIFICATION_RETENTION_DAYS`, 1 to 3650).
    pub retention_days: i32,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        NotificationConfig { retention_days: 90 }
    }
}

/// The approval SLA sweep of this server process (`WORKFLOW_APPROVAL_SWEEP*`,
/// approvals design SHAA-1869 §7.2); see
/// [`crate::modules::workflows::runtime::sweep`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalSweepConfig {
    /// `WORKFLOW_APPROVAL_SWEEP` (`on` / `off`): off leaves the sweep to other processes.
    pub enabled: bool,
    /// `WORKFLOW_APPROVAL_SWEEP_INTERVAL_SECS` (10 to 3600).
    pub interval: Duration,
}

impl Default for ApprovalSweepConfig {
    fn default() -> Self {
        ApprovalSweepConfig { enabled: true, interval: Duration::from_secs(60) }
    }
}

/// The workflow action outbox of this server process (`WORKFLOW_ACTIONS_*`,
/// design SHAA-2725 §4.2-§4.6); see [`crate::modules::workflows::actions::outbox`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowActionsConfig {
    /// `WORKFLOW_ACTIONS_WORKER` (`on` / `off`): off leaves fan-out and sending to other processes.
    pub worker: bool,
    /// `WORKFLOW_ACTIONS_CONCURRENCY` (1 to 32): runs and deliveries in flight in this process.
    pub concurrency: usize,
    /// `WORKFLOW_ACTIONS_POLL_MS` (100 to 60000): how often an idle worker looks for work.
    pub poll: Duration,
    /// `WORKFLOW_ACTIONS_MAX_ATTEMPTS` (1 to 20): attempts before a delivery is dead or a run
    /// is cancelled (`fan_out_failed`).
    pub max_attempts: i16,
    /// `WORKFLOW_ACTIONS_MAX_RECIPIENTS` (1 to 5000): users one run notifies, after expansion.
    pub max_recipients: usize,
    /// `WORKFLOW_ACTIONS_QUEUE_MAX` (10 to 10000000): pending runs and deliveries before new runs are suppressed.
    pub queue_max: i64,
    /// `WORKFLOW_ACTIONS_MAX_PER_INSTANCE_PER_HOUR` (1 to 10000): the loop breaker.
    pub max_per_instance_per_hour: i32,
    /// `WORKFLOW_ACTIONS_MAX_AGE_HOURS` (1 to 168): a run or delivery older than this gives up
    /// (`expired`), never sent late.
    pub max_age_hours: i32,
    /// `WORKFLOW_ACTIONS_RETENTION_DAYS` (1 to 3650): runs and delivered or skipped deliveries.
    pub retention_days: i32,
    /// `WORKFLOW_ACTIONS_DEAD_RETENTION_DAYS` (1 to 3650): dead deliveries.
    pub dead_retention_days: i32,
}

impl Default for WorkflowActionsConfig {
    fn default() -> Self {
        WorkflowActionsConfig {
            worker: true,
            concurrency: 4,
            poll: Duration::from_millis(1000),
            max_attempts: 8,
            max_recipients: 200,
            queue_max: 100_000,
            max_per_instance_per_hour: 50,
            max_age_hours: 24,
            retention_days: 30,
            dead_retention_days: 90,
        }
    }
}

/// The proxy webhook requests go through (`WEBHOOK_PROXY`, else
/// `HTTPS_PROXY` and `NO_PROXY`).
#[derive(Clone, Default, PartialEq, Eq)]
pub enum WebhookProxy {
    /// Direct connections: neither is set, or `WEBHOOK_PROXY=none`.
    #[default]
    Direct,
    /// `WEBHOOK_PROXY`: every webhook request.
    Explicit(url::Url),
    /// `HTTPS_PROXY` (or `https_proxy`), except for the hosts in `NO_PROXY`.
    FromEnv { url: url::Url, no_proxy: Option<String> },
}

impl std::fmt::Debug for WebhookProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Scheme, host and port only: `HTTPS_PROXY` may carry a password.
        let shown = |u: &url::Url| {
            format!("{}://{}:{}", u.scheme(), u.host_str().unwrap_or(""), u.port_or_known_default().unwrap_or(0))
        };
        match self {
            WebhookProxy::Direct => f.write_str("Direct"),
            WebhookProxy::Explicit(u) => write!(f, "Explicit({})", shown(u)),
            WebhookProxy::FromEnv { url, no_proxy } => write!(f, "FromEnv({}, no_proxy: {no_proxy:?})", shown(url)),
        }
    }
}

/// Outbound webhooks (`WEBHOOK*`, design SHAA-2725 §5): the operator's
/// ceiling over what administrators may configure, and how requests leave.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebhooksConfig {
    /// `WEBHOOKS_ALLOWED` (default false): off, no endpoint can be created or called.
    pub allowed: bool,
    /// `WEBHOOK_ALLOWED_HOSTS`: the hosts the administrator's allowlist must stay inside; unset, any host.
    pub allowed_hosts: Option<crate::modules::webhooks::hosts::HostCeiling>,
    /// `WEBHOOK_ALLOW_PRIVATE_CIDRS`: networks of the blocked ranges that may be reached (never cloud metadata).
    pub allow_private: Vec<ipnetwork::IpNetwork>,
    /// `WEBHOOK_ALLOW_HTTP` (default false): with an allowlist entry that allows it, plain http.
    pub allow_http: bool,
    pub proxy: WebhookProxy,
    /// `WEBHOOK_PROXY_PASSWORD_FILE`: the password for the proxy URL's user name.
    pub proxy_password_file: Option<PathBuf>,
    /// `WEBHOOK_TLS_CA_FILE`: a corporate CA trusted besides the public and OS roots.
    pub tls_ca_file: Option<PathBuf>,
}

/// How the SMTP connection is secured (`SMTP_SECURITY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpSecurity {
    /// Plain connection upgraded with STARTTLS, which the relay must offer (port 587).
    StartTls,
    /// TLS from the first byte (port 465).
    Tls,
    /// No encryption; only with `SMTP_ALLOW_PLAINTEXT=true`.
    None,
}

impl SmtpSecurity {
    pub fn as_str(self) -> &'static str {
        match self {
            SmtpSecurity::StartTls => "starttls",
            SmtpSecurity::Tls => "tls",
            SmtpSecurity::None => "none",
        }
    }
}

/// Outbound e-mail (`MAIL`, `SMTP_*`, `MAIL_*`; design SHAA-2725 §6.1). Set by
/// the operator, like every other outbound connection; the API never echoes
/// the user name, and the password is read from `SMTP_PASSWORD_FILE` when the
/// transport is built, never held here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailConfig {
    /// `MAIL=smtp`. Off, e-mail actions can be configured but their deliveries are skipped (`mail_off`).
    pub enabled: bool,
    pub host: Option<String>,
    pub port: u16,
    pub security: SmtpSecurity,
    /// `SMTP_TLS_CA_FILE`: a corporate CA trusted in addition to the public and OS roots.
    pub tls_ca_file: Option<PathBuf>,
    pub username: Option<String>,
    pub password_file: Option<PathBuf>,
    /// `MAIL_FROM` as given (validated as a mailbox).
    pub from: Option<String>,
    pub reply_to: Option<String>,
    /// `MAIL_DEFAULT_LOCALE`: `en` or `de`, for users without a language and for fixed addresses.
    pub default_locale: &'static str,
    /// `MAIL_ALLOW_EXTERNAL_ADDRESSES`: whether an action may name a fixed address at all.
    pub allow_external_addresses: bool,
    /// `MAIL_ALLOWED_DOMAINS`, lower-cased: the domains a fixed address may be in.
    pub allowed_domains: Vec<String>,
    /// `MAIL_MAX_PER_RECIPIENT_PER_HOUR`: more fold into one digest at the end of the hour.
    pub max_per_recipient_per_hour: i32,
    /// `SMTP_TIMEOUT_SECS`: per connection and per command.
    pub timeout: Duration,
    /// `PUBLIC_URL`: every link in a message is built from it.
    pub public_url: Option<String>,
}

impl Default for MailConfig {
    fn default() -> Self {
        MailConfig {
            enabled: false,
            host: None,
            port: 587,
            security: SmtpSecurity::StartTls,
            tls_ca_file: None,
            username: None,
            password_file: None,
            from: None,
            reply_to: None,
            default_locale: "en",
            allow_external_addresses: false,
            allowed_domains: Vec::new(),
            max_per_recipient_per_hour: 30,
            timeout: Duration::from_secs(15),
            public_url: None,
        }
    }
}

impl MailConfig {
    /// Whether an action may send to fixed address `address` on this server.
    pub fn address_allowed(&self, address: &str) -> bool {
        let domain = address.rsplit_once('@').map(|(_, d)| d.to_lowercase());
        self.allow_external_addresses && domain.is_some_and(|d| self.allowed_domains.contains(&d))
    }
}

/// A domain of `MAIL_ALLOWED_DOMAINS`: lower-case labels, at least two of them.
fn parse_mail_domain(raw: &str) -> Result<String, String> {
    let d = raw.trim().to_lowercase();
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    if d.len() > 253 || d.split('.').count() < 2 || !d.split('.').all(label_ok) {
        return Err(format!(
            "\"{raw}\" is not a domain name; list domains such as corp.example (IDNs in punycode, no wildcards)"
        ));
    }
    Ok(d)
}

/// Hard ceilings of the `BUSINESS_SERVICE_*` settings (the nesting ceiling is
/// also the database trigger's, migration 0033).
pub const BUSINESS_SERVICE_MAX_MEMBERS_CEILING: i64 = 50_000;
pub const BUSINESS_SERVICE_MAX_NESTING_CEILING: i32 = 8;

/// Hard ceilings of the `IMPACT_*` settings.
pub const IMPACT_MAX_DEPTH_CEILING: i32 = 20;
pub const IMPACT_MAX_NODES_CEILING: i32 = 10_000;
pub const IMPACT_TIMEOUT_MS_CEILING: u64 = 30_000;
/// Time an impact analysis has after `IMPACT_TIMEOUT_MS` to assemble its
/// result (in-edge counts, summaries, statuses), shared by all of those
/// queries: the analysis holds its connection for at most the two together.
pub const IMPACT_ASSEMBLY_ALLOWANCE_MS: u64 = 2_000;

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
    pub business_services: BusinessServiceConfig,
    pub exports: ExportConfig,
    pub notifications: NotificationConfig,
    pub approval_sweep: ApprovalSweepConfig,
    pub workflow_actions: WorkflowActionsConfig,
    pub webhooks: WebhooksConfig,
    pub mail: MailConfig,
}

/// The env file the variables were read from (`--env-file`, or `./.env`), as an absolute path.
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
            business_services,
            exports,
            notifications,
            approval_sweep,
            workflow_actions,
            webhooks,
            mail,
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
            .field("business_services", business_services)
            .field("exports", exports)
            .field("notifications", notifications)
            .field("approval_sweep", approval_sweep)
            .field("workflow_actions", workflow_actions)
            .field("webhooks", webhooks)
            .field("mail", mail)
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
///
/// A plain `http://` origin is refused unless it is loopback or `allow_http`
/// (`CORS_ALLOW_HTTP_ORIGINS=true`) says so (GH#445): whoever can tamper with
/// that origin's unencrypted traffic could inject script into its page and make
/// signed-in API calls through the user's browser.
fn parse_cors_origins(raw: &str, allow_http: bool) -> Result<Vec<String>, String> {
    let mut origins = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|o| !o.is_empty()) {
        // The URL parser accepts `*` in a host name, so wildcards are caught here.
        if entry.contains('*') {
            return Err(format!(
                "\"{entry}\" is not supported: wildcard origins match nothing. List each web UI origin \
                 explicitly, e.g. CORS_ORIGINS=https://cmdb.example.com,http://localhost:5173"
            ));
        }
        let url = url::Url::parse(entry)
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https") && u.username().is_empty() && u.password().is_none());
        let origin = url.as_ref().map(|u| u.origin().ascii_serialization());
        match origin {
            Some(o) if o == entry => {
                if url.as_ref().is_some_and(|u| u.scheme() == "http" && !crate::auth::sso::oidc::is_loopback(u))
                    && !allow_http
                {
                    return Err(format!(
                        "\"{entry}\" is a plain-HTTP origin: anyone who can intercept its traffic could make signed-in \
                         API calls. Serve the web UI over https, or set CORS_ALLOW_HTTP_ORIGINS=true to accept http:// \
                         origins (loopback ones such as http://localhost:5173 are always accepted)"
                    ));
                }
                origins.push(o);
            }
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

/// Parses `AUDIT_EXPORT`: `stdout`, `file:<path>`, `udp://host:port`, `tcp://host:port` or `tls://host:port`.
fn parse_audit_sink(raw: &str) -> Result<AuditSink, String> {
    const EXPECTED: &str =
        "expected off, stdout, file:/path/to/audit.log, udp://host:port, tcp://host:port or tls://host:port";
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
        "tls" => {
            // The name the certificate must carry: a DNS name or an IP address.
            let host = sink_host(addr);
            rustls_pki_types::ServerName::try_from(host)
                .map_err(|_| format!("\"{host}\" is not a valid host name or IP address for TLS"))?;
            Ok(AuditSink::Tls(addr.to_owned()))
        }
        _ => Err(format!("scheme \"{scheme}://\" is not supported; {EXPECTED}")),
    }
}

/// Reads the outbound e-mail settings. With `MAIL=smtp` the relay, the sender
/// and `PUBLIC_URL` are required: every message links to the web UI, and the
/// link is never built from a request's Host header.
fn read_mail(r: &mut Reader<'_>, public_url: Option<String>) -> MailConfig {
    let d = MailConfig::default();
    let enabled = r.one_of("MAIL", &["off", "smtp"], "off") == "smtp";
    let host = r.raw("SMTP_HOST").map(|h| h.trim().to_owned());
    if let Some(h) = &host
        && (h.contains(['/', ':', ' ', '@']) && h.parse::<std::net::Ipv6Addr>().is_err())
    {
        r.errors.push(format!("SMTP_HOST: \"{h}\" is not a host name or IP address; set the port in SMTP_PORT"));
    }
    let port = r.int::<u16>("SMTP_PORT", 1, 65535);
    let security = match r.one_of("SMTP_SECURITY", &["starttls", "tls", "none"], "starttls").as_str() {
        "tls" => SmtpSecurity::Tls,
        "none" => SmtpSecurity::None,
        _ => SmtpSecurity::StartTls,
    };
    let allow_plaintext = r.bool("SMTP_ALLOW_PLAINTEXT", false);
    if security == SmtpSecurity::None && !allow_plaintext {
        r.errors.push(
            "SMTP_SECURITY: none sends mail and any AUTH unencrypted; use starttls or tls, or set \
             SMTP_ALLOW_PLAINTEXT=true for a relay on a trusted network"
                .to_owned(),
        );
    }
    let tls_ca_file = r.raw("SMTP_TLS_CA_FILE").map(PathBuf::from);
    if tls_ca_file.is_some() && security == SmtpSecurity::None {
        r.errors.push("SMTP_TLS_CA_FILE: only used with SMTP_SECURITY=starttls or tls".to_owned());
    }
    let username = r.raw("SMTP_USERNAME");
    let password_file = r.raw("SMTP_PASSWORD_FILE").map(PathBuf::from);
    if username.is_some() != password_file.is_some() {
        r.errors.push(
            "SMTP_USERNAME: SMTP authentication needs both SMTP_USERNAME and SMTP_PASSWORD_FILE (the password is \
             read from the file, never from a variable)"
                .to_owned(),
        );
    }
    if username.is_some() && security == SmtpSecurity::None {
        r.errors.push(
            "SMTP_USERNAME: SMTP authentication is only sent over TLS; use SMTP_SECURITY=starttls or tls".to_owned(),
        );
    }
    let mailbox = |r: &mut Reader<'_>, key: &str| -> Option<String> {
        let raw = r.raw(key)?;
        match raw.trim().parse::<lettre::message::Mailbox>() {
            Ok(_) => Some(raw.trim().to_owned()),
            Err(e) => {
                r.errors.push(format!(
                    "{key}: \"{raw}\" is not an e-mail address ({e}); e.g. \"ShadouCMDB\" <cmdb-noreply@corp.example>"
                ));
                None
            }
        }
    };
    let from = mailbox(r, "MAIL_FROM");
    let reply_to = mailbox(r, "MAIL_REPLY_TO");
    let default_locale = if r.one_of("MAIL_DEFAULT_LOCALE", &["en", "de"], "en") == "de" { "de" } else { "en" };
    let allow_external_addresses = r.bool("MAIL_ALLOW_EXTERNAL_ADDRESSES", false);
    let mut allowed_domains = Vec::new();
    for raw in r.raw("MAIL_ALLOWED_DOMAINS").unwrap_or_default().split(',').filter(|s| !s.trim().is_empty()) {
        match parse_mail_domain(raw) {
            Ok(domain) => allowed_domains.push(domain),
            Err(e) => r.errors.push(format!("MAIL_ALLOWED_DOMAINS: {e}")),
        }
    }
    if allow_external_addresses && allowed_domains.is_empty() {
        r.errors.push(
            "MAIL_ALLOWED_DOMAINS: MAIL_ALLOW_EXTERNAL_ADDRESSES=true needs the domains fixed addresses may be in, \
             e.g. MAIL_ALLOWED_DOMAINS=corp.example"
                .to_owned(),
        );
    }
    let max_per_recipient_per_hour =
        r.int::<i32>("MAIL_MAX_PER_RECIPIENT_PER_HOUR", 1, 10_000).unwrap_or(d.max_per_recipient_per_hour);
    let timeout = r.int::<u64>("SMTP_TIMEOUT_SECS", 1, 300).map_or(d.timeout, Duration::from_secs);
    if enabled {
        for (key, missing) in
            [("SMTP_HOST", host.is_none()), ("MAIL_FROM", from.is_none() && r.raw("MAIL_FROM").is_none())]
        {
            if missing {
                r.errors.push(format!("{key}: required with MAIL=smtp"));
            }
        }
        if public_url.is_none() && r.raw("PUBLIC_URL").is_none() {
            r.errors.push(
                "MAIL: MAIL=smtp needs PUBLIC_URL, the address users open the web UI at: every message links to it, \
                 and the link is never built from a request"
                    .to_owned(),
            );
        }
    }
    MailConfig {
        enabled,
        host,
        port: port.unwrap_or(if security == SmtpSecurity::Tls { 465 } else { d.port }),
        security,
        tls_ca_file,
        username,
        password_file,
        from,
        reply_to,
        default_locale,
        allow_external_addresses,
        allowed_domains,
        max_per_recipient_per_hour,
        timeout,
        public_url,
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
        let body_timeout_secs = r.int::<u64>("HTTP_BODY_TIMEOUT_SECS", 1, 3600).unwrap_or(30);
        let send_timeout_secs = r.int::<u64>("HTTP_SEND_TIMEOUT_SECS", 1, 3600).unwrap_or(60);
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

        let cors_allow_http = r.bool("CORS_ALLOW_HTTP_ORIGINS", false);
        let cors_origins = match r.raw("CORS_ORIGINS").map(|s| parse_cors_origins(&s, cors_allow_http)) {
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
        let network_sink = sink.as_ref().is_some_and(|s| s.address().is_some());
        let format = match r
            .one_of("AUDIT_EXPORT_FORMAT", &["json", "rfc5424"], if network_sink { "rfc5424" } else { "json" })
            .as_str()
        {
            "rfc5424" => AuditFormat::Rfc5424,
            _ => AuditFormat::Json,
        };
        let facility = r.int::<u8>("AUDIT_SYSLOG_FACILITY", 0, 23).unwrap_or(13);
        let poll_ms = r.int::<u64>("AUDIT_EXPORT_POLL_MS", 100, 3_600_000).unwrap_or(2_000);
        let tls_ca_file = r.raw("AUDIT_EXPORT_TLS_CA_FILE").map(PathBuf::from);
        if tls_ca_file.is_some() && !matches!(sink, Some(AuditSink::Tls(_))) {
            r.errors.push("AUDIT_EXPORT_TLS_CA_FILE: only used with AUDIT_EXPORT=tls://host:port".to_owned());
        }
        let export = sink.map(|sink| AuditExportConfig {
            sink,
            format,
            facility,
            poll_interval: Duration::from_millis(poll_ms),
            tls_ca_file,
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
        if impact_timeout_ms + IMPACT_ASSEMBLY_ALLOWANCE_MS >= request_timeout_secs.saturating_mul(1_000) {
            r.errors.push(format!(
                "IMPACT_TIMEOUT_MS: {impact_timeout_ms} ms plus the {IMPACT_ASSEMBLY_ALLOWANCE_MS} ms an analysis \
                 has to assemble its result is not below HTTP_REQUEST_TIMEOUT_SECS ({request_timeout_secs} s), so \
                 a long impact analysis would time out instead of answering a truncated result"
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

        let service_defaults = BusinessServiceConfig::default();
        let business_services = BusinessServiceConfig {
            max_members: r
                .int::<i64>("BUSINESS_SERVICE_MAX_MEMBERS", 1, BUSINESS_SERVICE_MAX_MEMBERS_CEILING)
                .unwrap_or(service_defaults.max_members),
            max_nesting: r
                .int::<i32>("BUSINESS_SERVICE_MAX_NESTING", 1, BUSINESS_SERVICE_MAX_NESTING_CEILING)
                .unwrap_or(service_defaults.max_nesting),
        };

        // Each running export holds a pool connection for as long as the client
        // reads (GH#801). A value set here must leave POOL_RESERVE connections
        // when impact analyses and saved-view counts use their caps too.
        let exports = match r.int::<usize>("EXPORT_MAX_CONCURRENT", 1, 1_000) {
            Some(max_concurrent) => {
                let counts = crate::modules::saved_views::service::count_slots(pool_max);
                let used = impact_max_concurrent + counts + max_concurrent;
                if used + POOL_RESERVE > pool_max as usize {
                    r.errors.push(format!(
                        "EXPORT_MAX_CONCURRENT: {max_concurrent} exports, {impact_max_concurrent} impact analyses \
                         (IMPACT_MAX_CONCURRENT) and {counts} saved-view counts take {used} of the \
                         {pool_max} connections of DATABASE_POOL_MAX, leaving fewer than {POOL_RESERVE} for \
                         sign-in, /readyz and edits; lower EXPORT_MAX_CONCURRENT or IMPACT_MAX_CONCURRENT, or \
                         raise DATABASE_POOL_MAX"
                    ));
                }
                ExportConfig { max_concurrent }
            }
            None => ExportConfig::for_pool(pool_max),
        };

        let notifications = NotificationConfig {
            retention_days: r
                .int::<i32>("NOTIFICATION_RETENTION_DAYS", 1, 3650)
                .unwrap_or(NotificationConfig::default().retention_days),
        };

        let approval_sweep = ApprovalSweepConfig {
            enabled: r.one_of("WORKFLOW_APPROVAL_SWEEP", &["on", "off"], "on") == "on",
            interval: r
                .int::<u64>("WORKFLOW_APPROVAL_SWEEP_INTERVAL_SECS", 10, 3600)
                .map_or(ApprovalSweepConfig::default().interval, Duration::from_secs),
        };

        let d = WorkflowActionsConfig::default();
        let workflow_actions = WorkflowActionsConfig {
            worker: r.one_of("WORKFLOW_ACTIONS_WORKER", &["on", "off"], "on") == "on",
            concurrency: r.int::<usize>("WORKFLOW_ACTIONS_CONCURRENCY", 1, 32).unwrap_or(d.concurrency),
            poll: r.int::<u64>("WORKFLOW_ACTIONS_POLL_MS", 100, 60_000).map_or(d.poll, Duration::from_millis),
            max_attempts: r.int::<i16>("WORKFLOW_ACTIONS_MAX_ATTEMPTS", 1, 20).unwrap_or(d.max_attempts),
            max_recipients: r.int::<usize>("WORKFLOW_ACTIONS_MAX_RECIPIENTS", 1, 5000).unwrap_or(d.max_recipients),
            queue_max: r.int::<i64>("WORKFLOW_ACTIONS_QUEUE_MAX", 10, 10_000_000).unwrap_or(d.queue_max),
            max_per_instance_per_hour: r
                .int::<i32>("WORKFLOW_ACTIONS_MAX_PER_INSTANCE_PER_HOUR", 1, 10_000)
                .unwrap_or(d.max_per_instance_per_hour),
            max_age_hours: r.int::<i32>("WORKFLOW_ACTIONS_MAX_AGE_HOURS", 1, 168).unwrap_or(d.max_age_hours),
            retention_days: r.int::<i32>("WORKFLOW_ACTIONS_RETENTION_DAYS", 1, 3650).unwrap_or(d.retention_days),
            dead_retention_days: r
                .int::<i32>("WORKFLOW_ACTIONS_DEAD_RETENTION_DAYS", 1, 3650)
                .unwrap_or(d.dead_retention_days),
        };

        let webhooks = webhooks_config(&mut r);
        let mail = read_mail(&mut r, public_url.clone());

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
                body_timeout: Duration::from_secs(body_timeout_secs),
                send_timeout: Duration::from_secs(send_timeout_secs),
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
            business_services,
            exports,
            notifications,
            approval_sweep,
            workflow_actions,
            webhooks,
            mail,
        })
    }
}

/// The `WEBHOOK*` variables; `HTTPS_PROXY` and `NO_PROXY` only when `WEBHOOK_PROXY` is unset.
fn webhooks_config(r: &mut Reader<'_>) -> WebhooksConfig {
    let allowed_hosts = r.raw("WEBHOOK_ALLOWED_HOSTS").and_then(|s| {
        crate::modules::webhooks::hosts::HostCeiling::parse(&s)
            .map_err(|e| r.errors.push(format!("WEBHOOK_ALLOWED_HOSTS: {e}")))
            .ok()
    });
    let mut allow_private = Vec::new();
    for entry in r.raw("WEBHOOK_ALLOW_PRIVATE_CIDRS").unwrap_or_default().split(',').map(str::trim) {
        if entry.is_empty() {
            continue;
        }
        match entry.parse::<ipnetwork::IpNetwork>() {
            Ok(net) if net.prefix() == 0 => r.errors.push(format!(
                "WEBHOOK_ALLOW_PRIVATE_CIDRS: {entry} opens every address; list the networks webhook receivers are in"
            )),
            Ok(net) => allow_private.push(net),
            Err(_) => {
                r.errors.push(format!("WEBHOOK_ALLOW_PRIVATE_CIDRS: {entry:?} is not a CIDR (e.g. 10.20.0.0/16)"))
            }
        }
    }
    let proxy_url = |r: &mut Reader<'_>, key: &str, raw: &str| -> Option<url::Url> {
        match url::Url::parse(raw) {
            Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() => Some(u),
            _ => {
                r.errors.push(format!("{key}: expected an http:// or https:// proxy URL"));
                None
            }
        }
    };
    let proxy = match r.raw("WEBHOOK_PROXY") {
        Some(v) if v == "none" => WebhookProxy::Direct,
        Some(v) => match proxy_url(r, "WEBHOOK_PROXY", &v) {
            Some(u) if u.password().is_some() => {
                r.errors.push(
                    "WEBHOOK_PROXY: put the proxy password in WEBHOOK_PROXY_PASSWORD_FILE, not in the URL".to_owned(),
                );
                WebhookProxy::Direct
            }
            Some(u) => WebhookProxy::Explicit(u),
            None => WebhookProxy::Direct,
        },
        None => match r.raw("HTTPS_PROXY").or_else(|| r.raw("https_proxy")) {
            Some(v) => match proxy_url(r, "HTTPS_PROXY", &v) {
                Some(url) => WebhookProxy::FromEnv { url, no_proxy: r.raw("NO_PROXY").or_else(|| r.raw("no_proxy")) },
                None => WebhookProxy::Direct,
            },
            None => WebhookProxy::Direct,
        },
    };
    let proxy_password_file = r.raw("WEBHOOK_PROXY_PASSWORD_FILE").map(PathBuf::from);
    if proxy_password_file.is_some() && !matches!(&proxy, WebhookProxy::Explicit(u) if !u.username().is_empty()) {
        r.errors.push(
            "WEBHOOK_PROXY_PASSWORD_FILE: only used with WEBHOOK_PROXY=http://user@proxy:port (a user name in the URL)"
                .to_owned(),
        );
    }
    WebhooksConfig {
        allowed: r.bool("WEBHOOKS_ALLOWED", false),
        allowed_hosts,
        allow_private,
        allow_http: r.bool("WEBHOOK_ALLOW_HTTP", false),
        proxy,
        proxy_password_file,
        tls_ca_file: r.raw("WEBHOOK_TLS_CA_FILE").map(PathBuf::from),
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
            parse_cors_origins(
                " https://cmdb.example.com , http://localhost:5173,,http://127.0.0.1:8080,http://[::1]:3000",
                false
            )
            .unwrap(),
            ["https://cmdb.example.com", "http://localhost:5173", "http://127.0.0.1:8080", "http://[::1]:3000"]
        );
        assert_eq!(parse_cors_origins("", false).unwrap(), Vec::<String>::new());
    }

    /// GH#445: a non-loopback http:// origin needs CORS_ALLOW_HTTP_ORIGINS=true.
    #[test]
    fn cors_origins_refuse_plain_http_unless_allowed() {
        for bad in ["http://10.0.0.5:8080", "http://cmdb.example.com"] {
            let err = parse_cors_origins(&format!("https://cmdb.example.com,{bad}"), false).unwrap_err();
            assert!(err.contains(bad) && err.contains("CORS_ALLOW_HTTP_ORIGINS=true"), "{err}");
            assert_eq!(parse_cors_origins(bad, true).unwrap(), [bad]);
        }

        let err = load_with(&[("CORS_ORIGINS", "http://10.0.0.5:8080")]).unwrap_err().to_string();
        assert!(err.contains("CORS_ORIGINS") && err.contains("plain-HTTP"), "{err}");
        let cfg = load_with(&[("CORS_ORIGINS", "http://10.0.0.5:8080"), ("CORS_ALLOW_HTTP_ORIGINS", "true")]).unwrap();
        assert_eq!(cfg.cors_origins, ["http://10.0.0.5:8080"]);
        assert!(load_with(&[("CORS_ALLOW_HTTP_ORIGINS", "yes")]).is_err());
    }

    #[test]
    fn cors_origins_reject_wildcards_and_non_origins() {
        let err = parse_cors_origins("https://a.example.com,*", false).unwrap_err();
        assert!(err.contains("\"*\" is not supported"), "{err}");
        assert!(err.contains("explicitly"), "{err}");
        for bad in [
            "cmdb.example.com",
            "ftp://cmdb.example.com",
            "https://user@cmdb.example.com",
            "null",
            "https://*.example.com",
        ] {
            assert!(parse_cors_origins(bad, true).is_err(), "{bad} should be rejected");
        }
        // Would never match a browser's Origin header byte for byte; the error names the right spelling.
        assert!(
            parse_cors_origins("https://cmdb.example.com/", false)
                .unwrap_err()
                .contains("did you mean \"https://cmdb.example.com\"")
        );
        assert!(
            parse_cors_origins("https://CMDB.example.com", false).unwrap_err().contains("\"https://cmdb.example.com\"")
        );
        assert!(
            parse_cors_origins("https://cmdb.example.com:443", false)
                .unwrap_err()
                .contains("\"https://cmdb.example.com\"")
        );
        assert!(parse_cors_origins("https://cmdb.example.com/app", false).is_err());
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

    /// The approval sweep is on every 60 s by default; an operator turns it off
    /// per process or sets the interval, and a bad value is refused at start.
    #[test]
    fn approval_sweep_settings() {
        let cfg = load_with(&[]).unwrap();
        assert_eq!(cfg.approval_sweep, ApprovalSweepConfig { enabled: true, interval: Duration::from_secs(60) });
        let cfg =
            load_with(&[("WORKFLOW_APPROVAL_SWEEP", "off"), ("WORKFLOW_APPROVAL_SWEEP_INTERVAL_SECS", "15")]).unwrap();
        assert_eq!(cfg.approval_sweep, ApprovalSweepConfig { enabled: false, interval: Duration::from_secs(15) });
        for (key, bad) in [
            ("WORKFLOW_APPROVAL_SWEEP", "false"),
            ("WORKFLOW_APPROVAL_SWEEP_INTERVAL_SECS", "5"),
            ("WORKFLOW_APPROVAL_SWEEP_INTERVAL_SECS", "1m"),
        ] {
            let err = load_with(&[(key, bad)]).unwrap_err().to_string();
            assert!(err.contains(key), "{key}={bad}: {err}");
        }
    }

    /// The action outbox's settings: defaults, a full set, and refusals at start.
    #[test]
    fn workflow_actions_settings() {
        assert_eq!(load_with(&[]).unwrap().workflow_actions, WorkflowActionsConfig::default());
        let cfg = load_with(&[
            ("WORKFLOW_ACTIONS_WORKER", "off"),
            ("WORKFLOW_ACTIONS_CONCURRENCY", "8"),
            ("WORKFLOW_ACTIONS_POLL_MS", "250"),
            ("WORKFLOW_ACTIONS_MAX_ATTEMPTS", "3"),
            ("WORKFLOW_ACTIONS_MAX_RECIPIENTS", "50"),
            ("WORKFLOW_ACTIONS_QUEUE_MAX", "100"),
            ("WORKFLOW_ACTIONS_MAX_PER_INSTANCE_PER_HOUR", "5"),
            ("WORKFLOW_ACTIONS_MAX_AGE_HOURS", "2"),
            ("WORKFLOW_ACTIONS_RETENTION_DAYS", "7"),
            ("WORKFLOW_ACTIONS_DEAD_RETENTION_DAYS", "14"),
        ])
        .unwrap();
        assert_eq!(
            cfg.workflow_actions,
            WorkflowActionsConfig {
                worker: false,
                concurrency: 8,
                poll: Duration::from_millis(250),
                max_attempts: 3,
                max_recipients: 50,
                queue_max: 100,
                max_per_instance_per_hour: 5,
                max_age_hours: 2,
                retention_days: 7,
                dead_retention_days: 14,
            }
        );
        for (key, bad) in [
            ("WORKFLOW_ACTIONS_WORKER", "yes"),
            ("WORKFLOW_ACTIONS_CONCURRENCY", "0"),
            ("WORKFLOW_ACTIONS_POLL_MS", "50"),
            ("WORKFLOW_ACTIONS_MAX_ATTEMPTS", "21"),
            ("WORKFLOW_ACTIONS_QUEUE_MAX", "5"),
            ("WORKFLOW_ACTIONS_MAX_PER_INSTANCE_PER_HOUR", "0"),
        ] {
            let err = load_with(&[(key, bad)]).unwrap_err().to_string();
            assert!(err.contains(key), "{key}={bad}: {err}");
        }
    }

    /// Outbound e-mail: off by default; with MAIL=smtp the relay, the sender
    /// and PUBLIC_URL are required, and the unsafe combinations refuse to start.
    #[test]
    fn mail_settings() {
        assert_eq!(load_with(&[]).unwrap().mail, MailConfig::default());
        let smtp = [
            ("MAIL", "smtp"),
            ("SMTP_HOST", "relay.corp.example"),
            ("MAIL_FROM", "\"ShadouCMDB\" <cmdb@corp.example>"),
            ("PUBLIC_URL", "https://cmdb.corp.example/"),
        ];
        let cfg = load_with(&smtp).unwrap().mail;
        assert!(cfg.enabled);
        assert_eq!((cfg.port, cfg.security, cfg.default_locale), (587, SmtpSecurity::StartTls, "en"));
        assert_eq!(cfg.public_url.as_deref(), Some("https://cmdb.corp.example"));
        let cfg = load_with(&[
            ("SMTP_SECURITY", "tls"),
            ("MAIL_DEFAULT_LOCALE", "de"),
            ("MAIL_ALLOW_EXTERNAL_ADDRESSES", "true"),
            ("MAIL_ALLOWED_DOMAINS", "Corp.Example, lists.corp.example"),
            ("MAIL_MAX_PER_RECIPIENT_PER_HOUR", "5"),
            ("SMTP_TIMEOUT_SECS", "30"),
            ("SMTP_USERNAME", "cmdb"),
            ("SMTP_PASSWORD_FILE", "/run/secrets/smtp"),
        ])
        .unwrap()
        .mail;
        assert_eq!((cfg.port, cfg.default_locale, cfg.max_per_recipient_per_hour), (465, "de", 5));
        assert_eq!(cfg.allowed_domains, ["corp.example", "lists.corp.example"]);
        assert_eq!(cfg.timeout, Duration::from_secs(30));
        assert!(cfg.address_allowed("CAB@Corp.Example"));
        assert!(!cfg.address_allowed("cab@evil.example"));
        assert!(!MailConfig { allow_external_addresses: false, ..cfg }.address_allowed("cab@corp.example"));

        // PUBLIC_URL unset with MAIL=smtp refuses to start.
        let err = load_with(&smtp[..3]).unwrap_err().to_string();
        assert!(err.contains("MAIL: MAIL=smtp needs PUBLIC_URL"), "{err}");
        for (extra, key) in [
            (vec![("SMTP_HOST", "")], "SMTP_HOST: required"),
            (vec![("MAIL_FROM", "")], "MAIL_FROM: required"),
            (vec![("MAIL_FROM", "not an address")], "MAIL_FROM: \"not an address\""),
            (vec![("SMTP_HOST", "relay:25")], "SMTP_HOST"),
            (vec![("SMTP_SECURITY", "none")], "SMTP_SECURITY: none"),
            (vec![("SMTP_SECURITY", "ssl")], "SMTP_SECURITY"),
            (vec![("SMTP_USERNAME", "cmdb")], "SMTP_USERNAME: SMTP authentication needs both"),
            (
                vec![
                    ("SMTP_SECURITY", "none"),
                    ("SMTP_ALLOW_PLAINTEXT", "true"),
                    ("SMTP_USERNAME", "u"),
                    ("SMTP_PASSWORD_FILE", "/p"),
                ],
                "SMTP_USERNAME: SMTP authentication is only sent over TLS",
            ),
            (vec![("MAIL_ALLOW_EXTERNAL_ADDRESSES", "true")], "MAIL_ALLOWED_DOMAINS: MAIL_ALLOW_EXTERNAL"),
            (vec![("MAIL_ALLOWED_DOMAINS", "*.corp.example")], "MAIL_ALLOWED_DOMAINS: \"*.corp.example\""),
            (vec![("MAIL_DEFAULT_LOCALE", "fr")], "MAIL_DEFAULT_LOCALE"),
            (vec![("MAIL_MAX_PER_RECIPIENT_PER_HOUR", "0")], "MAIL_MAX_PER_RECIPIENT_PER_HOUR"),
        ] {
            let mut env: Vec<(&str, &str)> = smtp.to_vec();
            env.retain(|(k, _)| !extra.iter().any(|(e, _)| e == k));
            env.extend(extra.iter().copied());
            let err = load_with(&env).unwrap_err().to_string();
            assert!(err.contains(key), "{extra:?}: {err}");
        }
        // A plaintext relay is the operator's explicit choice.
        let mut env = smtp.to_vec();
        env.extend([("SMTP_SECURITY", "none"), ("SMTP_ALLOW_PLAINTEXT", "true")]);
        assert_eq!(load_with(&env).unwrap().mail.security, SmtpSecurity::None);
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
        assert_eq!(cfg.http.body_timeout, crate::http::DEFAULT_BODY_TIMEOUT);
        assert_eq!(cfg.http.send_timeout, Duration::from_secs(60));
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
        // The assembly allowance counts too: 1 s + 2 s is not below 3 s; 1.999 s + 2 s is below 4 s.
        let err = load_with(&[("IMPACT_TIMEOUT_MS", "1000"), ("HTTP_REQUEST_TIMEOUT_SECS", "3")]).unwrap_err();
        assert!(err.to_string().contains("IMPACT_TIMEOUT_MS"), "{err}");
        let ok = load_with(&[("IMPACT_TIMEOUT_MS", "1999"), ("HTTP_REQUEST_TIMEOUT_SECS", "4")]).unwrap();
        assert_eq!(ok.impact.timeout, Duration::from_millis(1999));
    }

    /// GH#801: exports hold a connection while the client reads; with impact
    /// analyses and saved-view counts at their caps, two connections stay free.
    #[test]
    fn export_limit_leaves_connections_for_the_rest_of_the_api() {
        assert_eq!(load_with(&[]).unwrap().exports.max_concurrent, 1, "default: DATABASE_POOL_MAX 10 / 8");
        assert_eq!(load_with(&[("DATABASE_POOL_MAX", "40")]).unwrap().exports.max_concurrent, 5);
        assert_eq!(load_with(&[("DATABASE_POOL_MAX", "2")]).unwrap().exports.max_concurrent, 1);
        // 40: impact 8 + counts 10 + exports 20 = 38, two left.
        let ok = load_with(&[("DATABASE_POOL_MAX", "40"), ("EXPORT_MAX_CONCURRENT", "20")]).unwrap();
        assert_eq!(ok.exports.max_concurrent, 20);
        for vars in [
            // 10: impact 5 + counts 2 + exports 2 = 9, one left.
            &[("EXPORT_MAX_CONCURRENT", "2")][..],
            &[("DATABASE_POOL_MAX", "4"), ("EXPORT_MAX_CONCURRENT", "1")][..],
            &[("DATABASE_POOL_MAX", "40"), ("EXPORT_MAX_CONCURRENT", "21")][..],
        ] {
            let err = load_with(vars).unwrap_err().to_string();
            assert!(err.contains("EXPORT_MAX_CONCURRENT") && err.contains("DATABASE_POOL_MAX"), "{vars:?}: {err}");
        }
        let err = load_with(&[("EXPORT_MAX_CONCURRENT", "0")]).unwrap_err();
        assert!(err.to_string().contains("EXPORT_MAX_CONCURRENT"), "{err}");
    }

    #[test]
    fn business_service_limits_have_ceilings() {
        let cfg = load_with(&[]).unwrap();
        assert_eq!(cfg.business_services, BusinessServiceConfig { max_members: 5_000, max_nesting: 5 });
        let ok = load_with(&[("BUSINESS_SERVICE_MAX_MEMBERS", "50000"), ("BUSINESS_SERVICE_MAX_NESTING", "8")]);
        assert_eq!(ok.unwrap().business_services, BusinessServiceConfig { max_members: 50_000, max_nesting: 8 });
        // Out of range stops the server; nothing is lowered to the ceiling.
        for (key, bad) in [
            ("BUSINESS_SERVICE_MAX_MEMBERS", "50001"),
            ("BUSINESS_SERVICE_MAX_MEMBERS", "0"),
            ("BUSINESS_SERVICE_MAX_NESTING", "9"),
            ("BUSINESS_SERVICE_MAX_NESTING", "0"),
        ] {
            let err = load_with(&[(key, bad)]).unwrap_err().to_string();
            assert!(err.contains(key), "{err}");
        }
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
        let e = export("tls://siem.example.com:6514").unwrap().unwrap();
        assert_eq!(e.sink, AuditSink::Tls("siem.example.com:6514".into()));
        assert_eq!((e.format, e.tls_ca_file), (AuditFormat::Rfc5424, None));
        let e = export("tls://[2001:db8::1]:6514").unwrap().unwrap();
        assert_eq!(e.sink.address().map(sink_host), Some("2001:db8::1"));
        let cfg =
            load_with(&[("AUDIT_EXPORT", "tls://siem:6514"), ("AUDIT_EXPORT_TLS_CA_FILE", "/etc/ca.pem")]).unwrap();
        assert_eq!(cfg.audit.export.unwrap().tls_ca_file, Some("/etc/ca.pem".into()));
        let err = load_with(&[("AUDIT_EXPORT", "tcp://siem:514"), ("AUDIT_EXPORT_TLS_CA_FILE", "/etc/ca.pem")]);
        assert!(err.unwrap_err().to_string().contains("AUDIT_EXPORT_TLS_CA_FILE"));
        for bad in [
            "syslog",
            "udp://siem.example.com",
            "http://siem:514",
            "tcp://:514",
            "udp://h:0",
            "file:",
            "tls://bad_name!:6514",
        ] {
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
