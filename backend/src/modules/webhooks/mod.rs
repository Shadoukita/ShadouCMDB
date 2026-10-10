//! Outbound webhooks (v0.4.0 design SHAA-2725 §5; slice S5).
//!
//! Administrators with `webhooks.manage` register endpoints and keep the host
//! allowlist; workflow designers point `webhook` actions at an endpoint by
//! key. The operator decides whether webhooks exist at all and which hosts and
//! networks they may reach (`WEBHOOKS_ALLOWED`, `WEBHOOK_ALLOWED_HOSTS`,
//! `WEBHOOK_ALLOW_PRIVATE_CIDRS`, `WEBHOOK_ALLOW_HTTP`), and how requests leave
//! (`WEBHOOK_PROXY`, `WEBHOOK_TLS_CA_FILE`).
//!
//! Requests are sent by the workflow action outbox ([`channel`]), never from
//! a request handler: signed (`X-ShadouCMDB-Signature`, [`signing`]), to the
//! addresses vetted at send time ([`ssrf`]), never following a redirect.
//! The payload is envelope v1 ([`envelope`], `payload.schema.json`); the
//! receiver's verification recipe is in `README.md` next to this file.

pub mod channel;
pub mod client;
pub mod envelope;
pub mod hosts;
#[cfg(test)]
mod qa_edge_tests;
pub mod service;
pub mod signing;
pub mod ssrf;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use axum::http::{Method, StatusCode};

use crate::api::route::{Body, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, route};
use crate::auth::permissions::GlobalPermission;
use crate::config::WebhooksConfig;
use crate::http::error::ErrorCode;
use crate::secrets::Keyring;
use service::{
    ListWebhookEndpointsQuery, WebhookAllowedHostCreate, WebhookEndpointCreate, WebhookEndpointUpdate,
    WebhookSecretRotate,
};

/// The webhook settings of this process and what sending needs: the TLS
/// roots, the proxy password, the resolver and the key for the sealed secrets.
pub struct Webhooks {
    pub cfg: WebhooksConfig,
    pub keyring: Arc<Keyring>,
    /// `PUBLIC_URL`, for the instance link in the payload.
    pub public_url: Option<String>,
    /// None while webhooks are off.
    tls: Option<Arc<rustls::ClientConfig>>,
    proxy_password: Option<String>,
    pub resolver: Arc<dyn ssrf::Resolve>,
}

impl std::fmt::Debug for Webhooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Webhooks").field("cfg", &self.cfg).field("public_url", &self.public_url).finish()
    }
}

impl Webhooks {
    /// Reads `WEBHOOK_TLS_CA_FILE` and `WEBHOOK_PROXY_PASSWORD_FILE` when webhooks are on.
    pub fn load(cfg: &WebhooksConfig, keyring: Arc<Keyring>, public_url: Option<String>) -> anyhow::Result<Webhooks> {
        let read = |var: &str, path: &std::path::Path| {
            std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("{var}: cannot read {}: {e}", path.display()))
        };
        let ca = cfg.tls_ca_file.as_deref().map(|p| read("WEBHOOK_TLS_CA_FILE", p)).transpose()?;
        let proxy_password = cfg
            .proxy_password_file
            .as_deref()
            .map(|p| read("WEBHOOK_PROXY_PASSWORD_FILE", p).map(|s| s.trim_end_matches(['\r', '\n']).to_owned()))
            .transpose()?;
        let tls = crate::auth::sso::tls::client_config(ca.as_deref())
            .map_err(|e| anyhow::anyhow!("WEBHOOK_TLS_CA_FILE: {e}"))?;
        Ok(Webhooks {
            cfg: cfg.clone(),
            keyring,
            public_url,
            tls: cfg.allowed.then(|| Arc::new(tls)),
            proxy_password,
            resolver: Arc::new(ssrf::SystemResolver),
        })
    }

    /// Webhooks off (the default of an app state built without settings).
    pub fn off(keyring: Arc<Keyring>) -> Webhooks {
        Webhooks {
            cfg: WebhooksConfig::default(),
            keyring,
            public_url: None,
            tls: None,
            proxy_password: None,
            resolver: Arc::new(ssrf::SystemResolver),
        }
    }

    /// Settings, a resolver and a CA for the test receivers.
    #[cfg(test)]
    pub fn for_tests(
        cfg: WebhooksConfig,
        keyring: Arc<Keyring>,
        resolver: Arc<dyn ssrf::Resolve>,
        ca_pem: Option<&str>,
    ) -> Webhooks {
        let tls = crate::auth::sso::tls::client_config(ca_pem).expect("test CA");
        Webhooks {
            tls: cfg.allowed.then(|| Arc::new(tls)),
            cfg,
            keyring,
            public_url: Some("https://cmdb.example.test".into()),
            proxy_password: None,
            resolver,
        }
    }
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub const TAG: &str = "Webhooks";
const ENDPOINTS: &str = "/api/v1/admin/webhook-endpoints";
const ENDPOINT: &str = "/api/v1/admin/webhook-endpoints/{id}";
const HOSTS: &str = "/api/v1/admin/webhook-allowed-hosts";
const DISABLED: &str = "409 WEBHOOKS_DISABLED while the operator has not set WEBHOOKS_ALLOWED=true.";

pub fn routes() -> Vec<Route> {
    let manage = GlobalPermission::WebhooksManage;
    vec![
        route(Method::GET, ENDPOINTS, "listWebhookEndpoints")
            .tag(TAG)
            .summary("List webhook endpoints (never their secrets)")
            .description(
                "For `webhooks.manage` holders. A caller with `workflows.manage` only (to pick an endpoint for a \
                 workflow action) gets the key, name and status of each, every other field null; anyone else 403.",
            )
            .class_checked()
            .handle(
                |api, In(NoPath, Query(q), NoBody): In<NoPath, Query<ListWebhookEndpointsQuery>, NoBody>| async move {
                    Ok(Json(service::list(&api.pool, &api.ctx, &q).await?))
                },
            ),
        route(Method::GET, ENDPOINT, "getWebhookEndpoint")
            .tag(TAG)
            .summary("Get one webhook endpoint (never its secret or header value)")
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::get(&api.pool, id).await?))
            }),
        route(Method::POST, ENDPOINTS, "createWebhookEndpoint")
            .tag(TAG)
            .summary("Register a webhook endpoint; the response shows its signing secret once")
            .description(format!(
                "The URL must be absolute https (http only when the operator set WEBHOOK_ALLOW_HTTP=true and the \
                 matching allowlist entry has `allowHttp`), without user name, password or fragment, and its host \
                 (and port) must be on the allowlist and inside the operator's WEBHOOK_ALLOWED_HOSTS: 400 \
                 VALIDATION_ERROR at `url` with `host_not_allowed`, `http_not_allowed`, `url_userinfo`, \
                 `url_fragment`, `invalid_url` or `too_long` otherwise. The server generates the signing secret \
                 (`whsec_...`) and returns it in this response only: copy it to the receiver now. `authHeader` is a \
                 static header sent with every request (e.g. `Authorization`); its value is write-only, and a name \
                 the server sets itself is refused (`reserved`). Where the request goes is judged again before every \
                 attempt, on the addresses the host resolves to then. {DISABLED}"
            ))
            .status(StatusCode::CREATED)
            .requires(manage)
            .recent_reauthentication()
            .errors(&[ErrorCode::Conflict, ErrorCode::WebhooksDisabled])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<WebhookEndpointCreate>>| async move {
                Ok(Json(service::create(&api.pool, &api.ctx, &api.webhooks, &b).await?))
            }),
        route(Method::PATCH, ENDPOINT, "updateWebhookEndpoint")
            .tag(TAG)
            .summary("Change a webhook endpoint (partial)")
            .description(format!(
                "A new `url` meets the rules of createWebhookEndpoint. `authHeader`: an object replaces the header, \
                 null removes it, left out keeps it. The status changes only through pause and resume. {DISABLED}"
            ))
            .requires(manage)
            .errors(&[ErrorCode::NotFound, ErrorCode::VersionConflict, ErrorCode::WebhooksDisabled])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<WebhookEndpointUpdate>>| async move {
                    Ok(Json(service::update(&api.pool, &api.ctx, &api.webhooks, id, &b).await?))
                },
            ),
        route(Method::DELETE, ENDPOINT, "deleteWebhookEndpoint")
            .tag(TAG)
            .summary("Delete a webhook endpoint that no workflow action uses")
            .description(
                "409 IN_USE while a workflow action names it (the message lists them). Its deliveries still \
                 waiting die (`endpoint_deleted`), each audited as `workflow.action_dead`.",
            )
            .requires(manage)
            .errors(&[ErrorCode::NotFound, ErrorCode::InUse])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                service::remove(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
        route(Method::POST, "/api/v1/admin/webhook-endpoints/{id}/rotate-secret", "rotateWebhookSecret")
            .tag(TAG)
            .summary("Replace the signing secret; the response shows the new one once")
            .description(format!(
                "For `graceHours` (default 24, 0 to 168) every request carries a second `v1=` signature made with \
                 the previous secret, so the receiver can switch without missing a request. An endpoint suspended \
                 with `secret_required` becomes paused: share the new secret with the receiver, then resume it. \
                 {DISABLED}"
            ))
            .requires(manage)
            .recent_reauthentication()
            .errors(&[ErrorCode::NotFound, ErrorCode::WebhooksDisabled])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<WebhookSecretRotate>>| async move {
                    Ok(Json(service::rotate(&api.pool, &api.ctx, &api.webhooks, id, &b).await?))
                },
            ),
        route(Method::POST, "/api/v1/admin/webhook-endpoints/{id}/ping", "pingWebhookEndpoint")
            .tag(TAG)
            .summary("Send a signed test request (`ping`) and say what came of it")
            .description(format!(
                "Runs the URL rules and the address checks a delivery runs, then sends one `ping` event (also to a \
                 paused or suspended endpoint). Answers 200 with `ok` and the reason a delivery would record \
                 (`host_not_allowed`, `address_blocked:<ip>`, `redirect_not_followed`, ...). Nothing is stored and \
                 the circuit breaker does not count it. {DISABLED}"
            ))
            .requires(manage)
            .errors(&[ErrorCode::NotFound, ErrorCode::WebhooksDisabled])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(channel::ping(&api.pool, &api.webhooks, id).await?))
            }),
        route(Method::POST, "/api/v1/admin/webhook-endpoints/{id}/pause", "pauseWebhookEndpoint")
            .tag(TAG)
            .summary("Pause an endpoint: its deliveries are held until it is resumed")
            .description(
                "Held deliveries older than WORKFLOW_ACTIONS_MAX_AGE_HOURS die as `endpoint_suspended`. Audited as \
                 `webhook_endpoint.suspend` with reason `paused`. A paused or suspended endpoint is left as it is.",
            )
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::pause(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::POST, "/api/v1/admin/webhook-endpoints/{id}/resume", "resumeWebhookEndpoint")
            .tag(TAG)
            .summary("Resume a paused or suspended endpoint and send its held deliveries")
            .description(format!(
                "Its URL must still be allowed (400 `host_not_allowed` otherwise) and its secrets must decrypt: an \
                 endpoint created by a configuration import, restored under another key or reset with \
                 `webhooks reset-undecryptable` needs a secret rotation first (422 SECRET_REQUIRED). The failure \
                 count starts again at 0. {DISABLED}"
            ))
            .requires(manage)
            .errors(&[ErrorCode::NotFound, ErrorCode::SecretRequired, ErrorCode::WebhooksDisabled])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::resume(&api.pool, &api.ctx, &api.webhooks, id).await?))
            }),
        route(Method::GET, HOSTS, "listWebhookAllowedHosts")
            .tag(TAG)
            .summary("The hosts webhooks may reach (an empty list allows none)")
            .description("Answers `{ data }` with every entry; the list is short and not paginated.")
            .requires(manage)
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(service::list_hosts(&api.pool).await?))
            }),
        route(Method::POST, HOSTS, "createWebhookAllowedHost")
            .tag(TAG)
            .summary("Allow webhooks to a host")
            .description(format!(
                "An exact host, `*.domain` (any name one label or more below it; never a bare `*`) or an IP address \
                 (matched only literally), with an optional port. When the operator set WEBHOOK_ALLOWED_HOSTS, the \
                 entry must lie inside it (400 `host_not_allowed`), and `allowHttp` needs WEBHOOK_ALLOW_HTTP=true \
                 (400 `http_not_allowed`). 409 CONFLICT for an entry that exists. {DISABLED}"
            ))
            .status(StatusCode::CREATED)
            .requires(manage)
            .recent_reauthentication()
            .errors(&[ErrorCode::Conflict, ErrorCode::WebhooksDisabled])
            .handle(
                |api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<WebhookAllowedHostCreate>>| async move {
                    Ok(Json(service::create_host(&api.pool, &api.ctx, &api.webhooks, &b).await?))
                },
            ),
        route(Method::DELETE, "/api/v1/admin/webhook-allowed-hosts/{id}", "deleteWebhookAllowedHost")
            .tag(TAG)
            .summary("Remove an allowlist entry; endpoints it alone allowed are suspended")
            .description(
                "Every endpoint whose URL no remaining entry allows is suspended with reason `host_not_allowed` \
                 (audited) and listed in the response; its deliveries are held.",
            )
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(service::delete_host(&api.pool, &api.ctx, &api.webhooks, id).await?))
            }),
    ]
}
