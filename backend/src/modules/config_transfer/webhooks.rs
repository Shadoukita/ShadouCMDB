//! Webhook endpoints and the host allowlist in a configuration file (version
//! 14, design SHAA-2725 §12). Secrets never travel: an exported endpoint has
//! `authHeaderSet` only; an imported new one gets a signing secret nobody has
//! seen and stays suspended (`secret_required`) until an administrator rotates
//! it, shares it with the receiver and resumes it. An existing endpoint keeps
//! its secrets; only its other fields change.

use std::collections::HashSet;

use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::format::{ConfigFile, WebhookAllowedHostSpec, WebhookEndpointSpec};
use super::{ChangeAction, FieldChange, ImportWarning, Importer, not_in_file};
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::AppError;
use crate::modules::webhooks::Webhooks;
use crate::modules::webhooks::hosts::{HostMatch, HostPattern, check_url, parse_url};
use crate::modules::webhooks::service::{self, EndpointRow, WebhookEndpointStatus};
use crate::secrets::sealed::{self, EndpointSecret};

/// The endpoints and allowlist as a file holds them.
pub(super) async fn snapshot(
    conn: &mut PgConnection,
) -> Result<(Vec<WebhookEndpointSpec>, Vec<WebhookAllowedHostSpec>), AppError> {
    let endpoints: Vec<EndpointRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM cmdb.webhook_endpoints ORDER BY key",
        service::COLUMNS
    )))
    .fetch_all(&mut *conn)
    .await?;
    let hosts: Vec<(String, Option<i32>, bool, Option<String>)> = sqlx::query_as(
        "SELECT host_pattern, port, allow_http, comment FROM cmdb.webhook_allowed_hosts
         ORDER BY host_pattern, port NULLS FIRST",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok((
        endpoints.iter().map(spec).collect(),
        hosts
            .into_iter()
            .map(|(host_pattern, port, allow_http, comment)| WebhookAllowedHostSpec {
                host_pattern,
                port,
                allow_http,
                comment,
            })
            .collect(),
    ))
}

fn spec(e: &EndpointRow) -> WebhookEndpointSpec {
    WebhookEndpointSpec {
        key: e.key.clone(),
        name: e.name.clone(),
        url: e.url.clone(),
        timeout_ms: e.timeout_ms,
        max_per_minute: e.max_per_minute,
        max_in_flight: e.max_in_flight,
        auth_header_name: e.auth_header_name.clone(),
        auth_header_set: e.auth_header_ciphertext.is_some(),
    }
}

fn change(field: &str, from: Value, to: Value) -> Option<FieldChange> {
    (from != to).then(|| FieldChange { field: field.into(), from, to })
}

fn audit(
    entity_type: &'static str,
    action: AuditAction,
    id: Uuid,
    old: Option<Value>,
    new: Option<Value>,
) -> AuditEntry {
    AuditEntry { action, entity_type, entity_id: id, old_value: old, new_value: new }
}

impl Importer<'_> {
    /// The allowlist first (merged by host and port, never deleted), then the
    /// endpoints (by key, never deleted), so a URL is judged against the list
    /// the file brings.
    pub(super) async fn webhooks(
        &mut self,
        w: &Webhooks,
        file: &ConfigFile,
        current_endpoints: &[WebhookEndpointSpec],
        current_hosts: &[WebhookAllowedHostSpec],
        warnings: &mut Vec<ImportWarning>,
    ) -> Result<(), AppError> {
        if let Some(list) = &file.webhook_allowed_hosts {
            self.allowed_hosts(w, list, current_hosts, warnings).await?;
        }
        if let Some(list) = &file.webhook_endpoints {
            self.endpoints(w, list, current_endpoints, warnings).await?;
        }
        Ok(())
    }

    async fn allowed_hosts(
        &mut self,
        w: &Webhooks,
        list: &[WebhookAllowedHostSpec],
        current: &[WebhookAllowedHostSpec],
        warnings: &mut Vec<ImportWarning>,
    ) -> Result<(), AppError> {
        const SECTION: &str = "webhookAllowedHosts";
        let label = |pattern: &str, port: Option<i32>| match port {
            Some(p) => format!("{pattern}:{p}"),
            None => pattern.to_owned(),
        };
        let mut in_file = HashSet::new();
        let mut checked = Vec::with_capacity(list.len());
        for (i, h) in list.iter().enumerate() {
            let path = format!("{SECTION}.{i}");
            let host = HostMatch::parse(&h.host_pattern).map_err(|e| {
                AppError::field(format!("{path}.hostPattern"), format!("Host pattern {e}"), "invalid_format")
            })?;
            if h.port.is_some_and(|p| !(1..=65535).contains(&p)) {
                return Err(AppError::field(format!("{path}.port"), "1 to 65,535", "out_of_range"));
            }
            if h.comment.as_ref().is_some_and(|c| c.chars().count() > 500) {
                return Err(AppError::field(format!("{path}.comment"), "At most 500 characters", "too_long"));
            }
            in_file.insert(label(&host.stored(), h.port));
            checked.push((path, host, h));
        }
        let here: Vec<String> = current.iter().map(|c| label(&c.host_pattern, c.port)).collect();
        self.section(SECTION, not_in_file(here.iter(), &in_file));
        let (_, by) = crate::modules::workflows::service::actor(self.ctx);
        for (path, host, h) in checked {
            let stored = host.stored();
            let key = label(&stored, h.port);
            let pattern = HostPattern { host, port: h.port.and_then(|p| u16::try_from(p).ok()) };
            if let Some(ceiling) = &w.cfg.allowed_hosts
                && !ceiling.contains(&pattern)
            {
                warnings.push(ImportWarning {
                    path,
                    message: format!(
                        "Allowlist entry {key} was not imported: it lies outside the hosts this server's operator \
                         allows (WEBHOOK_ALLOWED_HOSTS)"
                    ),
                });
                continue;
            }
            let allow_http = h.allow_http && w.cfg.allow_http;
            if h.allow_http && !allow_http {
                warnings.push(ImportWarning {
                    path: format!("{path}.allowHttp"),
                    message: format!(
                        "Allowlist entry {key} is imported without allowHttp: this server's operator does not allow \
                         plain http (WEBHOOK_ALLOW_HTTP=false)"
                    ),
                });
            }
            let comment = h.comment.as_deref().map(str::trim).filter(|c| !c.is_empty());
            let existing: Option<(Uuid, bool, Option<String>)> = sqlx::query_as(
                "SELECT id, allow_http, comment FROM cmdb.webhook_allowed_hosts
                 WHERE host_pattern = $1 AND port IS NOT DISTINCT FROM $2 FOR UPDATE",
            )
            .bind(&stored)
            .bind(h.port)
            .fetch_optional(&mut *self.conn)
            .await?;
            let new_value =
                json!({ "hostPattern": stored, "port": h.port, "allowHttp": allow_http, "comment": comment });
            match existing {
                Some((id, old_http, old_comment)) => {
                    let fields: Vec<FieldChange> = [
                        change("allowHttp", json!(old_http), json!(allow_http)),
                        change("comment", json!(old_comment), json!(comment)),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    if fields.is_empty() {
                        self.record(SECTION, key, None, Vec::new());
                        continue;
                    }
                    sqlx::query("UPDATE cmdb.webhook_allowed_hosts SET allow_http = $2, comment = $3 WHERE id = $1")
                        .bind(id)
                        .bind(allow_http)
                        .bind(comment)
                        .execute(&mut *self.conn)
                        .await?;
                    let old = json!({ "hostPattern": stored, "port": h.port, "allowHttp": old_http,
                        "comment": old_comment });
                    crud::write_audit(
                        self.conn,
                        self.ctx,
                        vec![audit("webhook_allowed_hosts", AuditAction::Update, id, Some(old), Some(new_value))],
                    )
                    .await?;
                    self.record(SECTION, key, Some(ChangeAction::Update), fields);
                }
                None => {
                    let id: Uuid = sqlx::query_scalar(
                        "INSERT INTO cmdb.webhook_allowed_hosts (host_pattern, port, allow_http, comment, created_by_name)
                         VALUES ($1, $2, $3, $4, $5) RETURNING id",
                    )
                    .bind(&stored)
                    .bind(h.port)
                    .bind(allow_http)
                    .bind(comment)
                    .bind(&by)
                    .fetch_one(&mut *self.conn)
                    .await?;
                    crud::write_audit(
                        self.conn,
                        self.ctx,
                        vec![audit("webhook_allowed_hosts", AuditAction::Create, id, None, Some(new_value))],
                    )
                    .await?;
                    self.record(SECTION, key, Some(ChangeAction::Create), Vec::new());
                }
            }
        }
        Ok(())
    }

    async fn endpoints(
        &mut self,
        w: &Webhooks,
        list: &[WebhookEndpointSpec],
        current: &[WebhookEndpointSpec],
        warnings: &mut Vec<ImportWarning>,
    ) -> Result<(), AppError> {
        const SECTION: &str = "webhookEndpoints";
        let mut keys = HashSet::new();
        for (i, e) in list.iter().enumerate() {
            let path = format!("{SECTION}.{i}");
            let key_ok = e.key.len() <= 63
                && e.key.starts_with(|c: char| c.is_ascii_lowercase())
                && e.key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
            if !key_ok {
                return Err(AppError::field(
                    format!("{path}.key"),
                    "Lower case letters, digits, _ and -",
                    "invalid_format",
                ));
            }
            if !keys.insert(e.key.clone()) {
                return Err(AppError::field(
                    format!("{path}.key"),
                    format!("Key {} is listed twice", e.key),
                    "duplicate",
                ));
            }
            let name = e.name.trim();
            if name.is_empty() || name.chars().count() > 100 {
                return Err(AppError::field(format!("{path}.name"), "1 to 100 characters", "invalid_format"));
            }
            parse_url(&e.url).map_err(|r| AppError::field(format!("{path}.url"), r.message, r.code))?;
            for (field, ok) in [
                ("timeoutMs", (1000..=30_000).contains(&e.timeout_ms)),
                ("maxPerMinute", (1..=6000).contains(&e.max_per_minute)),
                ("maxInFlight", (1..=16).contains(&e.max_in_flight)),
            ] {
                if !ok {
                    return Err(AppError::field(format!("{path}.{field}"), "Out of range", "out_of_range"));
                }
            }
        }
        let here: Vec<String> = current.iter().map(|e| e.key.clone()).collect();
        self.section(SECTION, not_in_file(here.iter(), &keys));
        let allowlist = service::allowlist(self.conn).await?;
        for (i, e) in list.iter().enumerate() {
            let path = format!("{SECTION}.{i}");
            let url = parse_url(&e.url).map_err(|r| AppError::field(format!("{path}.url"), r.message, r.code))?;
            let allowed = check_url(url.as_str(), w.ceiling(), &allowlist);
            let existing: Option<EndpointRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT {} FROM cmdb.webhook_endpoints WHERE key = $1 FOR UPDATE",
                service::COLUMNS
            )))
            .bind(&e.key)
            .fetch_optional(&mut *self.conn)
            .await?;
            match existing {
                Some(before) => {
                    let was = spec(&before);
                    let fields: Vec<FieldChange> = [
                        change("name", json!(was.name), json!(e.name.trim())),
                        change("url", json!(was.url), json!(url.as_str())),
                        change("timeoutMs", json!(was.timeout_ms), json!(e.timeout_ms)),
                        change("maxPerMinute", json!(was.max_per_minute), json!(e.max_per_minute)),
                        change("maxInFlight", json!(was.max_in_flight), json!(e.max_in_flight)),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    if e.auth_header_set && before.auth_header_ciphertext.is_none() {
                        warnings.push(ImportWarning {
                            path: format!("{path}.authHeaderSet"),
                            message: format!(
                                "Endpoint {} has no auth header here and the file cannot carry its value: set it \
                                 under Administration > Webhooks",
                                e.key
                            ),
                        });
                    }
                    if fields.is_empty() {
                        self.record(SECTION, e.key.clone(), None, Vec::new());
                        continue;
                    }
                    sqlx::query(
                        "UPDATE cmdb.webhook_endpoints SET name = $2, url = $3, timeout_ms = $4, max_per_minute = $5,
                           max_in_flight = $6, version = version + 1 WHERE id = $1",
                    )
                    .bind(before.id)
                    .bind(e.name.trim())
                    .bind(url.as_str())
                    .bind(e.timeout_ms)
                    .bind(e.max_per_minute)
                    .bind(e.max_in_flight)
                    .execute(&mut *self.conn)
                    .await?;
                    let after = service::row(self.conn, before.id, false).await?;
                    crud::write_audit(
                        self.conn,
                        self.ctx,
                        vec![audit(
                            "webhook_endpoints",
                            AuditAction::Update,
                            before.id,
                            Some(before.audited()),
                            Some(after.audited()),
                        )],
                    )
                    .await?;
                    if let Err(refused) = &allowed
                        && after.status != WebhookEndpointStatus::Suspended
                    {
                        service::suspend(self.conn, self.ctx, before.id, "host_not_allowed").await?;
                        warnings.push(ImportWarning {
                            path: format!("{path}.url"),
                            message: format!("Endpoint {} is suspended: {}", e.key, refused.message),
                        });
                    }
                    self.record(SECTION, e.key.clone(), Some(ChangeAction::Update), fields);
                }
                None => {
                    let id = Uuid::new_v4();
                    let sealed = sealed::seal_endpoint_secret(
                        &w.keyring,
                        id,
                        EndpointSecret::Secret,
                        &crate::modules::webhooks::signing::generate(),
                    );
                    sqlx::query(
                        "INSERT INTO cmdb.webhook_endpoints (id, key, name, url, timeout_ms, max_per_minute,
                           max_in_flight, secret_ciphertext, secret_key_id, status, suspended_reason)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'suspended', 'secret_required')",
                    )
                    .bind(id)
                    .bind(&e.key)
                    .bind(e.name.trim())
                    .bind(url.as_str())
                    .bind(e.timeout_ms)
                    .bind(e.max_per_minute)
                    .bind(e.max_in_flight)
                    .bind(&sealed.bytes)
                    .bind(sealed.key_id.0)
                    .execute(&mut *self.conn)
                    .await?;
                    let after = service::row(self.conn, id, false).await?;
                    crud::write_audit(
                        self.conn,
                        self.ctx,
                        vec![audit("webhook_endpoints", AuditAction::Create, id, None, Some(after.audited()))],
                    )
                    .await?;
                    let mut message = format!(
                        "Endpoint {} is created suspended: its signing secret never travels in a file. Rotate the \
                         secret under Administration > Webhooks, share it with the receiver, then resume the endpoint",
                        e.key
                    );
                    if e.auth_header_set {
                        message.push_str("; set its auth header there as well");
                    }
                    if let Err(refused) = &allowed {
                        message.push_str(&format!(". Its URL is not allowed here yet: {}", refused.message));
                    }
                    warnings.push(ImportWarning { path, message });
                    self.record(SECTION, e.key.clone(), Some(ChangeAction::Create), Vec::new());
                }
            }
        }
        Ok(())
    }
}
