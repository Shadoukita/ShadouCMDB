//! Outbound e-mail (v0.4.0 design SHAA-2725 §6; slice S4).
//!
//! The operator configures the relay in the environment (`MAIL`, `SMTP_*`,
//! `MAIL_*`, see [`crate::config::MailConfig`]); nothing about it is stored
//! in the database or echoed by the API beyond the host, port, security and
//! sender. The transport is `lettre`'s pooled SMTP client over rustls with
//! ring: certificate chain and host name are always verified, against the
//! Mozilla roots, the operating system's store and `SMTP_TLS_CA_FILE`.
//!
//! Messages are written by the workflow action e-mail channel
//! ([`crate::modules::workflows::actions::email`]) with the templates of
//! [`render`]; this module only sends them and keeps the last outcome for
//! `GET /admin/mail/status`. `/readyz` never depends on the relay: mail being
//! down must not take the CMDB out of the load balancer.

pub mod render;

use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use axum::http::Method;
use chrono::{DateTime, Utc};
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{Certificate, CertificateStore, Tls, TlsParameters, TlsVersion};
use lettre::transport::smtp::extension::ClientId;
use lettre::transport::smtp::{AsyncSmtpTransport, PoolConfig};
use lettre::{AsyncTransport, Tokio1Executor};
use regex::Regex;
use serde::Serialize;
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::context::RequestContext;
use crate::api::route::{In, Json, NoBody, NoPath, NoQuery, Route, route};
use crate::auth::permissions::GlobalPermission;
use crate::config::{MailConfig, SmtpSecurity};
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode};

pub const TAG: &str = "Mail";

/// The longest error text kept for the status (the relay's reply, our wording).
const MAX_STATUS_ERROR: usize = 500;

/// Why a message was not accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// Worth another attempt: no connection, TLS or DNS failure, a timeout, an SMTP 4xx reply.
    Transient { code: Option<u16>, message: String },
    /// Never worth another for this recipient: an SMTP 5xx reply.
    Permanent { code: Option<u16>, message: String },
}

impl SendError {
    pub fn code(&self) -> Option<u16> {
        match self {
            SendError::Transient { code, .. } | SendError::Permanent { code, .. } => *code,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            SendError::Transient { message, .. } | SendError::Permanent { message, .. } => message,
        }
    }
}

/// What the status reports about the last attempts of this process.
#[derive(Debug, Default, Clone)]
struct LastOutcome {
    success_at: Option<DateTime<Utc>>,
    error_at: Option<DateTime<Utc>>,
    error: Option<String>,
}

/// The mail settings of this process and, with `MAIL=smtp`, its transport.
pub struct Mail {
    pub cfg: MailConfig,
    transport: Option<AsyncSmtpTransport<Tokio1Executor>>,
    from: Option<Mailbox>,
    reply_to: Option<Mailbox>,
    /// The right-hand side of every `Message-ID`: `PUBLIC_URL`'s host.
    id_host: String,
    last: Mutex<LastOutcome>,
}

impl Default for Mail {
    /// Mail off.
    fn default() -> Self {
        Mail {
            cfg: MailConfig::default(),
            transport: None,
            from: None,
            reply_to: None,
            id_host: "localhost".into(),
            last: Mutex::default(),
        }
    }
}

impl std::fmt::Debug for Mail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mail").field("cfg", &self.cfg).finish_non_exhaustive()
    }
}

/// The host of `PUBLIC_URL`, or `localhost`.
fn public_host(cfg: &MailConfig) -> String {
    cfg.public_url
        .as_deref()
        .and_then(|u| url::Url::parse(u).ok())
        .and_then(|u| u.host_str().map(|h| h.trim_matches(['[', ']']).to_owned()))
        .unwrap_or_else(|| "localhost".into())
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

impl Mail {
    /// Builds the transport for `MAIL=smtp`: reads `SMTP_PASSWORD_FILE` and
    /// `SMTP_TLS_CA_FILE` now, so a missing or broken file stops the server at
    /// start rather than at the first message. Nothing connects yet.
    pub fn build(cfg: &MailConfig, concurrency: usize) -> anyhow::Result<Mail> {
        let id_host = public_host(cfg);
        if !cfg.enabled {
            return Ok(Mail { cfg: cfg.clone(), id_host, ..Mail::default() });
        }
        let host = cfg.host.clone().ok_or_else(|| anyhow::anyhow!("SMTP_HOST: required with MAIL=smtp"))?;
        let from = cfg
            .from
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("MAIL_FROM: required with MAIL=smtp"))?
            .parse::<Mailbox>()
            .map_err(|e| anyhow::anyhow!("MAIL_FROM: {e}"))?;
        let reply_to = cfg
            .reply_to
            .as_deref()
            .map(str::parse::<Mailbox>)
            .transpose()
            .map_err(|e| anyhow::anyhow!("MAIL_REPLY_TO: {e}"))?;
        let tls = match cfg.security {
            SmtpSecurity::None => {
                if !is_loopback(&host) {
                    tracing::warn!(
                        host = %host,
                        "SMTP_SECURITY=none: workflow e-mail goes to the relay unencrypted (SMTP_ALLOW_PLAINTEXT=true)"
                    );
                }
                Tls::None
            }
            security => {
                let params = tls_parameters(&host, cfg)?;
                if security == SmtpSecurity::Tls { Tls::Wrapper(params) } else { Tls::Required(params) }
            }
        };
        let hello = match id_host.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(ip)) => ClientId::Ipv4(ip),
            Ok(std::net::IpAddr::V6(ip)) => ClientId::Ipv6(ip),
            Err(_) => ClientId::Domain(id_host.clone()),
        };
        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
            .port(cfg.port)
            .tls(tls)
            .hello_name(hello)
            .timeout(Some(cfg.timeout))
            .pool_config(PoolConfig::new().max_size(u32::try_from(concurrency.max(1)).unwrap_or(4)));
        if let (Some(user), Some(file)) = (&cfg.username, &cfg.password_file) {
            let password = std::fs::read_to_string(file)
                .map_err(|e| anyhow::anyhow!("SMTP_PASSWORD_FILE: cannot read {}: {e}", file.display()))?;
            let password = password.trim_end_matches(['\r', '\n']).to_owned();
            if password.is_empty() {
                anyhow::bail!("SMTP_PASSWORD_FILE: {} is empty", file.display());
            }
            builder = builder
                .credentials(Credentials::new(user.clone(), password))
                .authentication(vec![Mechanism::Plain, Mechanism::Login]);
        }
        Ok(Mail {
            cfg: cfg.clone(),
            transport: Some(builder.build()),
            from: Some(from),
            reply_to,
            id_host,
            last: Mutex::default(),
        })
    }

    /// A transport to `host:port` without TLS (tests: the SMTP sink).
    #[cfg(test)]
    pub fn plain_for_tests(cfg: &MailConfig, host: &str, port: u16) -> Mail {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
            .port(port)
            .tls(Tls::None)
            .timeout(Some(cfg.timeout))
            .build();
        Mail {
            cfg: cfg.clone(),
            transport: Some(transport),
            from: cfg.from.as_deref().and_then(|f| f.parse().ok()),
            reply_to: cfg.reply_to.as_deref().and_then(|f| f.parse().ok()),
            id_host: public_host(cfg),
            last: Mutex::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.transport.is_some()
    }

    pub fn from(&self) -> Option<&Mailbox> {
        self.from.as_ref()
    }

    pub fn reply_to(&self) -> Option<&Mailbox> {
        self.reply_to.as_ref()
    }

    /// The `Message-ID` of a delivery: stable across its retries, so a relay
    /// or client drops a resend after a lost `250`.
    pub fn message_id(&self, delivery: Uuid) -> String {
        format!("<{delivery}@{}>", self.id_host)
    }

    /// How long one message may take, connection included.
    pub fn send_timeout(&self) -> Duration {
        // Connect, greeting, EHLO, STARTTLS, AUTH, MAIL, RCPT, DATA, end of data: each may take `timeout`.
        self.cfg.timeout.saturating_mul(4)
    }

    /// Sends one message; never inside a database transaction.
    pub async fn send(&self, message: lettre::Message) -> Result<(), SendError> {
        let Some(transport) = &self.transport else {
            return Err(SendError::Permanent { code: None, message: "Outbound e-mail is off (MAIL=off)".into() });
        };
        let outcome = match tokio::time::timeout(self.send_timeout(), transport.send(message)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(classify(&e)),
            Err(_) => Err(SendError::Transient { code: None, message: "The SMTP relay did not answer in time".into() }),
        };
        self.note(&outcome);
        outcome
    }

    fn note(&self, outcome: &Result<(), SendError>) {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        match outcome {
            Ok(()) => last.success_at = Some(Utc::now()),
            Err(e) => {
                last.error_at = Some(Utc::now());
                last.error =
                    Some(capped(&format!("{}{}", e.code().map(|c| format!("{c} ")).unwrap_or_default(), e.message())));
            }
        }
    }

    /// The settings and the last outcomes, as `getMailStatus` shows them.
    pub fn status(&self) -> MailStatus {
        let last = self.last.lock().unwrap_or_else(|e| e.into_inner()).clone();
        MailStatus {
            enabled: self.cfg.enabled,
            host: self.cfg.host.clone(),
            port: self.cfg.enabled.then_some(self.cfg.port),
            security: self.cfg.enabled.then(|| self.cfg.security.as_str().to_owned()),
            from: self.from.as_ref().map(ToString::to_string),
            default_locale: self.cfg.default_locale.to_owned(),
            external_addresses: self.cfg.allow_external_addresses,
            max_per_recipient_per_hour: self.cfg.max_per_recipient_per_hour,
            last_success_at: last.success_at,
            last_error_at: last.error_at,
            last_error: last.error,
        }
    }
}

/// `c***@corp.example`: enough to tell lists apart, not to harvest them (N-Q3).
/// A quoted local part (`"carol smith"@corp.example`) may itself hold an `@`,
/// so the domain is what follows the last one, and the first character kept
/// is the one inside the quotes.
pub fn mask(address: &str) -> String {
    match address.rsplit_once('@') {
        Some((local, domain)) => {
            format!("{}***@{domain}", local.trim_start_matches('"').chars().next().unwrap_or('*'))
        }
        None => "***".into(),
    }
}

/// An address as a relay may quote it: a quoted local part (escapes allowed)
/// or a bare one, then a domain; both may be non-ASCII (SMTPUTF8, IDN), so
/// they run up to the delimiters rather than over an ASCII class (GH#883).
static ADDRESS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:"(?:[^"\\\r\n]|\\.)*"|[^\s<>()\[\],;:@"]+)@[^\s<>()\[\],;:@"]+"#).expect("address regex")
});

/// `text` with every e-mail address in it masked: a relay's error names the
/// recipient (`550 5.1.1 <carol@corp.example>`), so no error we keep (the
/// delivery's `last_error`, the mail status) may show what the API masks
/// everywhere else. Masking a masked address leaves it as it is.
pub fn mask_addresses(text: &str) -> String {
    ADDRESS_RE.replace_all(text, |c: &regex::Captures<'_>| mask(&c[0])).into_owned()
}

/// A relay error as kept: on one line, addresses masked (GH#881), cut short.
fn capped(s: &str) -> String {
    let one_line: String = mask_addresses(s).chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    one_line.chars().take(MAX_STATUS_ERROR).collect()
}

/// The relay's reply as an outcome: 4xx and anything that never got a reply
/// (connection, TLS, timeout) are transient; 5xx is permanent.
fn classify(e: &lettre::transport::smtp::Error) -> SendError {
    let code = e.status().map(u16::from);
    let message = capped(&e.to_string());
    if e.is_permanent() { SendError::Permanent { code, message } } else { SendError::Transient { code, message } }
}

/// TLS for the relay: the Mozilla roots, the operating system's store and
/// `SMTP_TLS_CA_FILE`; TLS 1.2 at least; verification always on.
fn tls_parameters(host: &str, cfg: &MailConfig) -> anyhow::Result<TlsParameters> {
    let mut builder = TlsParameters::builder(host.to_owned())
        .certificate_store(CertificateStore::WebpkiRoots)
        .set_min_tls_version(TlsVersion::Tlsv12);
    // Only certificates rustls accepts as anchors: one broken file in the OS
    // store must not stop mail.
    let mut probe = rustls::RootCertStore::empty();
    for cert in crate::auth::sso::tls::native_roots() {
        if probe.add(cert.clone()).is_ok()
            && let Ok(c) = Certificate::from_der(cert.to_vec())
        {
            builder = builder.add_root_certificate(c);
        }
    }
    if let Some(file) = &cfg.tls_ca_file {
        let pem = std::fs::read_to_string(file)
            .map_err(|e| anyhow::anyhow!("SMTP_TLS_CA_FILE: cannot read {}: {e}", file.display()))?;
        for cert in crate::auth::sso::tls::parse_ca_pem(&pem).map_err(|e| anyhow::anyhow!("SMTP_TLS_CA_FILE: {e}"))? {
            let c = Certificate::from_der(cert.to_vec()).map_err(|e| anyhow::anyhow!("SMTP_TLS_CA_FILE: {e}"))?;
            builder = builder.add_root_certificate(c);
        }
    }
    builder.build_rustls().map_err(|e| anyhow::anyhow!("SMTP TLS settings: {e}"))
}

// ---------------------------------------------------------------------------
// API
// ---------------------------------------------------------------------------

/// The outbound e-mail settings of the server process that answered, and how its last attempts went
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MailStatus {
    /// `MAIL=smtp`; when false, e-mail actions can be configured but nothing is sent (`mail_off`)
    pub enabled: bool,
    /// The SMTP relay (`SMTP_HOST`)
    pub host: Option<String>,
    pub port: Option<u16>,
    /// `starttls`, `tls` or `none`
    pub security: Option<String>,
    /// The sender (`MAIL_FROM`)
    pub from: Option<String>,
    /// `en` or `de`: the language of users without one and of fixed addresses (`MAIL_DEFAULT_LOCALE`)
    pub default_locale: String,
    /// Whether actions may send to fixed addresses (`MAIL_ALLOW_EXTERNAL_ADDRESSES`)
    pub external_addresses: bool,
    /// More e-mails to one recipient within an hour fold into one digest (`MAIL_MAX_PER_RECIPIENT_PER_HOUR`)
    pub max_per_recipient_per_hour: i32,
    /// The last message this process handed to the relay, since it started
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error_at: Option<DateTime<Utc>>,
    /// The last failure of this process: the relay's reply code and text, or the connection error
    pub last_error: Option<String>,
}

/// The outcome of a test message
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MailTestResult {
    /// The caller's own address, the only one a test goes to
    pub to: String,
    /// Whether the relay accepted the message (it may still bounce later)
    pub sent: bool,
    /// The relay's reply code when it refused
    pub smtp_code: Option<u16>,
    /// The relay's reply or the connection error when it was not sent
    pub error: Option<String>,
}

fn may_see_mail(ctx: &RequestContext) -> Result<(), AppError> {
    ctx.require(GlobalPermission::WorkflowsManage)
        .or_else(|e| ctx.require(GlobalPermission::WebhooksManage).map_err(|_| e))
}

/// Sends a test message to the caller's own address, at once (not through the outbox).
async fn test(api: &crate::api::route::Api) -> Result<MailTestResult, AppError> {
    let ctx = &api.ctx;
    may_see_mail(ctx)?;
    let mail = &api.mail;
    if !mail.enabled() {
        return Err(AppError::new(
            ErrorCode::MailNotConfigured,
            "Outbound e-mail is off (MAIL=off); the operator sets MAIL=smtp and the SMTP_* variables",
        ));
    }
    let principal = ctx.principal().ok_or_else(crate::api::context::unauthenticated)?;
    let mut conn = api.pool.acquire().await?;
    let (email, display_name, locale): (Option<String>, String, Option<String>) =
        sqlx::query_as("SELECT email, display_name, locale FROM cmdb.users WHERE id = $1")
            .bind(principal.user_id)
            .fetch_one(&mut *conn)
            .await?;
    let Some(email) = email.filter(|e| !e.trim().is_empty()) else {
        return Err(AppError::new(ErrorCode::Conflict, "Your account has no e-mail address to send a test message to"));
    };
    let locale = render::Locale::of(locale.as_deref(), mail.cfg.default_locale);
    let rendered =
        render::test_message(locale, &principal.username, mail.cfg.public_url.as_deref().unwrap_or_default());
    let to = Mailbox::new(
        Some(display_name),
        email.parse().map_err(|_| {
            AppError::new(ErrorCode::Conflict, "Your account's e-mail address cannot be used as a recipient")
        })?,
    );
    let message = render::message(mail, Uuid::new_v4(), to, &rendered).map_err(|e| {
        tracing::error!(error = %e, "cannot build the mail test message");
        AppError::internal()
    })?;
    let outcome = mail.send(message).await;
    let entry = AuditEntry {
        action: AuditAction::MailTest,
        entity_type: "users",
        entity_id: principal.user_id,
        old_value: None,
        new_value: Some(json!({ "to": principal.username, "sent": outcome.is_ok(),
            "smtpCode": outcome.as_ref().err().and_then(SendError::code) })),
    };
    crud::write_audit(&mut conn, ctx, vec![entry]).await?;
    Ok(MailTestResult {
        to: email,
        sent: outcome.is_ok(),
        smtp_code: outcome.as_ref().err().and_then(SendError::code),
        error: outcome.err().map(|e| e.message().to_owned()),
    })
}

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/admin/mail/status", "getMailStatus")
            .tag(TAG)
            .summary("Outbound e-mail: the settings and the last outcomes")
            .description(
                "Needs `workflows.manage` or `webhooks.manage`. The operator sets outbound e-mail in the environment \
                 (`MAIL`, `SMTP_*`, `MAIL_*`); this shows the relay, the security and the sender, never the user \
                 name or password. `lastSuccessAt` and `lastError` are those of the server process that answered, \
                 since it started.",
            )
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                may_see_mail(&api.ctx)?;
                Ok(Json(api.mail.status()))
            }),
        route(Method::POST, "/api/v1/admin/mail/test", "sendMailTest")
            .tag(TAG)
            .summary("Send a test message to your own address")
            .description(
                "Needs `workflows.manage` or `webhooks.manage`, and a session. Sends at once, not through the \
                 workflow outbox, to the caller's own e-mail address only, in the caller's language. Audited as \
                 `mail.test` on the caller. A refusal by the relay is reported in the result (`sent` false, \
                 `smtpCode`, `error`), not as an error status. 409 MAIL_NOT_CONFIGURED with `MAIL=off`; 409 \
                 CONFLICT when the caller has no address.",
            )
            .session_only()
            .errors(&[ErrorCode::MailNotConfigured, ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(test(&api).await?))
            }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GH#881: the status shows the relay's reply with the recipient masked.
    #[test]
    fn status_last_error_masks_addresses() {
        let mail = Mail::default();
        mail.note(&Err(SendError::Permanent {
            code: Some(550),
            message: "5.1.1 <carol@corp.example>: Recipient address rejected".into(),
        }));
        let error = mail.status().last_error.expect("last error");
        assert!(!error.contains("carol@corp.example"), "lastError leaks the address: {error}");
        assert_eq!(error, "550 5.1.1 <c***@corp.example>: Recipient address rejected");
    }

    #[test]
    fn masking_is_idempotent() {
        let once = mask_addresses("to carol@corp.example and bob.smith@mail.corp.example");
        assert_eq!(once, "to c***@corp.example and b***@mail.corp.example");
        assert_eq!(mask_addresses(&once), once);
    }

    /// GH#883: non-ASCII and quoted local parts are masked as a whole.
    #[test]
    fn masking_covers_utf8_and_quoted_local_parts() {
        for (text, masked) in [
            ("550 <jörg@corp.example>", "550 <j***@corp.example>"),
            ("550 <carol@bücher.example>", "550 <c***@bücher.example>"),
            (r#"550 <"carol smith"@corp.example>"#, "550 <c***@corp.example>"),
            (r#"550 <"carol@home"@corp.example>"#, "550 <c***@corp.example>"),
            (r#"550 <"carol \"cs\" smith"@corp.example>"#, "550 <c***@corp.example>"),
            (r#"550 <""@corp.example>"#, "550 <****@corp.example>"),
            (
                "rcpt carol@corp.example, bob@corp.example: rejected",
                "rcpt c***@corp.example, b***@corp.example: rejected",
            ),
        ] {
            let once = mask_addresses(text);
            assert_eq!(once, masked, "masking {text}");
            assert_eq!(mask_addresses(&once), once, "re-masking {text}");
        }
    }
}
