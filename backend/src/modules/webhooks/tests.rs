//! Webhooks end to end against local test receivers (SHAA-2735, design
//! SHAA-2725 slice S5), on a scratch database: the API and what it never
//! shows (secret, header value), the SSRF vectors refused at send time, the
//! signature and its rotation grace, http only with both switches, redirects
//! not followed, the proxy, the circuit breaker, the per-endpoint claim
//! limits, a key rotation that re-encrypts the secrets, and an import that
//! creates endpoints suspended until their secret is rotated.

#![allow(clippy::type_complexity)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use uuid::Uuid;

use super::ssrf::tests::StaticResolver;
use super::{Webhooks, channel, signing};
use crate::api::context::RequestContext;
use crate::config::{WebhookProxy, WebhooksConfig, WorkflowActionsConfig};
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app_with_webhooks, call, code};
use crate::modules::workflows::actions::WorkflowActionKind;
use crate::modules::workflows::actions::outbox::{self, Claimed};
use crate::modules::workflows::runtime_tests::{DEFS, World, world_with_app};
use crate::secrets::Keyring;

const ENDPOINTS: &str = "/api/v1/admin/webhook-endpoints";
const HOSTS: &str = "/api/v1/admin/webhook-allowed-hosts";
const HEADER_VALUE: &str = "Bearer header-value-7f3a9c";

// ---------------------------------------------------------------------------
// Test receivers
// ---------------------------------------------------------------------------

/// A request a receiver got.
#[derive(Debug, Clone)]
struct Hit {
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Hit {
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map(String::as_str).unwrap_or_default()
    }
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

/// A local HTTP or HTTPS receiver: records what it gets, answers as told.
#[derive(Clone)]
struct Receiver {
    port: u16,
    hits: Arc<Mutex<Vec<Hit>>>,
    answer: Arc<Mutex<(u16, Vec<(&'static str, String)>)>>,
    /// The response body: `None` for `{"received":true}`, [`ECHO`] for the request's own body.
    reply: Arc<Mutex<Option<String>>>,
}

/// A [`Receiver`] reply that repeats the request body.
const ECHO: &str = "\u{0}echo";

impl Receiver {
    async fn start(tls: Option<Arc<tokio_rustls::rustls::ServerConfig>>) -> Receiver {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let r = Receiver {
            port: listener.local_addr().unwrap().port(),
            hits: Arc::default(),
            answer: Arc::new(Mutex::new((200, Vec::new()))),
            reply: Arc::default(),
        };
        let shared = r.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (r, tls) = (shared.clone(), tls.clone());
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                        let r = r.clone();
                        async move { Ok::<_, std::convert::Infallible>(r.handle(req).await) }
                    });
                    let http = hyper::server::conn::http1::Builder::new();
                    match tls {
                        Some(cfg) => {
                            if let Ok(s) = tokio_rustls::TlsAcceptor::from(cfg).accept(tcp).await {
                                let _ = http.serve_connection(TokioIo::new(s), svc).await;
                            }
                        }
                        None => {
                            let _ = http.serve_connection(TokioIo::new(tcp), svc).await;
                        }
                    }
                });
            }
        });
        r
    }

    async fn handle(&self, req: hyper::Request<Incoming>) -> hyper::Response<Full<Bytes>> {
        let headers = req
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or_default().to_owned()))
            .collect();
        let body = req.into_body().collect().await.map(|b| b.to_bytes().to_vec()).unwrap_or_default();
        let reply = match self.reply.lock().unwrap().clone() {
            None => b"{\"received\":true}".to_vec(),
            Some(r) if r == ECHO => body.clone(),
            Some(r) => r.into_bytes(),
        };
        self.hits.lock().unwrap().push(Hit { headers, body });
        let (status, extra) = self.answer.lock().unwrap().clone();
        let mut res = hyper::Response::builder().status(status);
        for (k, v) in extra {
            res = res.header(k, v);
        }
        res.body(Full::new(Bytes::from(reply))).unwrap()
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    fn answer(&self, status: u16, headers: Vec<(&'static str, String)>) {
        *self.answer.lock().unwrap() = (status, headers);
    }
}

/// A CA and a server certificate for `name`, signed by it: (CA PEM, server config).
fn tls_server(name: &str) -> (String, Arc<tokio_rustls::rustls::ServerConfig>) {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
    use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_pem = ca_params.self_signed(&ca_key).unwrap().pem();
    let issuer = Issuer::new(ca_params, ca_key);
    let key = KeyPair::generate().unwrap();
    let cert = CertificateParams::new(vec![name.to_owned()]).unwrap().signed_by(&key, &issuer).unwrap();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = tokio_rustls::rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.der().clone()], key)
        .unwrap();
    (ca_pem, Arc::new(config))
}

/// A local CONNECT proxy that tunnels every target to `127.0.0.1:target`, recording each request head.
async fn connect_proxy(target: u16) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let mut client = tokio::io::BufReader::new(tcp);
                let mut head = String::new();
                loop {
                    let mut line = String::new();
                    if client.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                log.lock().unwrap().push(head.clone());
                if !head.starts_with("CONNECT ") {
                    return;
                }
                let Ok(mut upstream) = tokio::net::TcpStream::connect(("127.0.0.1", target)).await else { return };
                let mut client = client.into_inner();
                if client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.is_ok() {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                }
            });
        }
    });
    (port, seen)
}

// ---------------------------------------------------------------------------
// Set-up and helpers
// ---------------------------------------------------------------------------

fn on(private: &[&str]) -> WebhooksConfig {
    WebhooksConfig {
        allowed: true,
        allow_private: private.iter().map(|n| n.parse().unwrap()).collect(),
        ..WebhooksConfig::default()
    }
}

fn hooks(cfg: WebhooksConfig, resolver: &Arc<StaticResolver>, ca: Option<&str>) -> Arc<Webhooks> {
    Arc::new(Webhooks::for_tests(cfg, Keyring::for_tests(), resolver.clone(), ca))
}

struct Env {
    db: scratch::Scratch,
    w: World,
    hooks: Arc<Webhooks>,
    resolver: Arc<StaticResolver>,
}

/// A world whose API runs with `cfg`; `hook.example.test` resolves to the local receivers.
async fn env(name: &str, cfg: WebhooksConfig, ca: Option<&str>) -> Option<Env> {
    let db = scratch::database(name).await?;
    let resolver = Arc::new(StaticResolver::default());
    resolver.set("hook.example.test", &["127.0.0.1"]);
    let hooks = hooks(cfg, &resolver, ca);
    let w = world_with_app(&db, app_with_webhooks(db.pool.clone(), hooks.clone())).await;
    Some(Env { db, w, hooks, resolver })
}

impl Env {
    async fn allow(&self, pattern: &str, port: Option<u16>, http: bool) -> Value {
        self.w.ok("POST", HOSTS, json!({ "hostPattern": pattern, "port": port, "allowHttp": http })).await
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.w.call(&self.w.admin, method, path, body).await
    }

    /// Creates endpoint `key` at `url`: (endpoint id, secret).
    async fn endpoint(&self, key: &str, url: &str, extra: Value) -> (Uuid, String) {
        let mut body = json!({ "key": key, "name": format!("Endpoint {key}"), "url": url });
        if let Value::Object(more) = extra {
            body.as_object_mut().unwrap().extend(more);
        }
        let v = self.w.ok("POST", ENDPOINTS, body).await;
        (v["endpoint"]["id"].as_str().unwrap().parse().unwrap(), v["secret"].as_str().unwrap().to_owned())
    }

    async fn ping(&self, id: Uuid) -> Value {
        let (status, v) = self.call("POST", &format!("{ENDPOINTS}/{id}/ping"), None).await;
        assert_eq!(status, 200, "{v}");
        v
    }

    /// A `sync` webhook action on `approve` to endpoint `key`, listing `fields`.
    async fn action(&self, key: &str, fields: Value) {
        let path = format!("{DEFS}/{}/actions", self.w.definition);
        let version = self.w.ok("GET", &path, json!(null)).await["version"].clone();
        let v = self
            .w
            .ok(
                "PUT",
                &path,
                json!({ "version": version, "actions": [{ "key": "sync", "name": "Sync", "kind": "webhook",
                    "trigger": "transition", "transition": "approve", "endpoint": key,
                    "settings": { "includeAttributes": fields } }] }),
            )
            .await;
        assert_eq!(v["actions"][0]["endpoint"]["key"], key, "{v}");
    }

    /// Runs `approve` on a new CI, then fans the run out: the delivery's id.
    async fn approve(&self, approver: &Creds) -> Uuid {
        let ci = self
            .w
            .ok(
                "POST",
                "/api/v1/configuration-items",
                json!({ "classId": self.w.server, "attributes": { "environment": "prod", "owner_team": "ops" } }),
            )
            .await;
        let ci: Uuid = ci["id"].as_str().unwrap().parse().unwrap();
        let (status, v) = self.w.start(&self.w.admin, ci).await;
        assert_eq!(status, 201, "{v}");
        let instance: Uuid = v["instance"]["id"].as_str().unwrap().parse().unwrap();
        let body = json!({ "transitionKey": "approve", "expectedVersion": 1,
            "fields": { "owner_team": "ops", "risk": 1 }, "comment": "CAB ok" });
        let (status, v) = self.w.transition(approver, instance, body).await;
        assert_eq!(status, 200, "{v}");
        let cfg = WorkflowActionsConfig::default();
        for id in outbox::claim_runs(&self.w.pool, "test", 100).await.unwrap() {
            outbox::fan_out(&self.w.pool, &cfg, id, "test").await.unwrap();
        }
        sqlx::query_scalar(
            "SELECT d.id FROM workflow_action_deliveries d JOIN workflow_action_runs r ON r.id = d.run_id
             WHERE r.ci_id = $1",
        )
        .bind(ci)
        .fetch_one(&self.w.pool)
        .await
        .unwrap()
    }
}

/// Sends every due webhook delivery once, as a worker would; returns how many.
async fn send_all(pool: &PgPool, hooks: &Arc<Webhooks>) -> usize {
    let channel = channel::channel(pool.clone(), hooks.clone());
    let cfg = WorkflowActionsConfig::default();
    let claimed =
        outbox::claim_deliveries(pool, "test", WorkflowActionKind::Webhook, channel.timeout, 100).await.unwrap();
    for c in &claimed {
        let outcome = (channel.send)(c.clone()).await;
        assert!(outbox::record(pool, &cfg, c, outcome).await.unwrap());
    }
    claimed.len()
}

async fn delivery(pool: &PgPool, id: Uuid) -> (String, Option<String>, Option<i32>) {
    sqlx::query_as("SELECT status, status_reason, last_status_code FROM workflow_action_deliveries WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

fn details(v: &Value) -> Vec<(String, String)> {
    crate::modules::workflows::runtime_tests::details(v)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The API refuses what §5.2 refuses, never shows the secret or the header
/// value after creation (responses, audit rows, export, log), and gives a
/// `workflows.manage`-only caller key, name and status only. Webhooks off:
/// 409 WEBHOOKS_DISABLED. GH#836: the database refuses credentials in the URL
/// and ciphertexts too short to be sealed.
#[tokio::test]
async fn endpoints_are_validated_and_never_show_their_secrets() {
    let (logs, _guard) = crate::auth::setup_token::capture::json();
    let Some(e) = env("webhooks_api", on(&["127.0.0.0/8"]), None).await else { return };
    let w = &e.w;

    // Deny by default: the empty allowlist allows nothing.
    let (status, v) = e
        .call("POST", ENDPOINTS, Some(json!({ "key": "itsm", "name": "ITSM", "url": "https://hook.example.test/" })))
        .await;
    assert_eq!((status, details(&v)), (400, vec![("url".into(), "host_not_allowed".into())]), "{v}");
    e.allow("hook.example.test", None, false).await;
    let (status, v) = e.call("POST", HOSTS, Some(json!({ "hostPattern": "*" }))).await;
    assert_eq!((status, details(&v)), (400, vec![("hostPattern".into(), "invalid_format".into())]), "{v}");
    let (status, v) = e.call("POST", HOSTS, Some(json!({ "hostPattern": "hook.example.test" }))).await;
    assert_eq!((status, code(&v)), (409, "CONFLICT"), "{v}");
    let (status, v) =
        e.call("POST", HOSTS, Some(json!({ "hostPattern": "plain.example.test", "allowHttp": true }))).await;
    assert_eq!((status, details(&v)), (400, vec![("allowHttp".into(), "http_not_allowed".into())]), "{v}");

    for (url, reason) in [
        ("https://user:pw@hook.example.test/", "url_userinfo"),
        ("https://user@hook.example.test/", "url_userinfo"),
        ("https://hook.example.test/#frag", "url_fragment"),
        ("http://hook.example.test/", "http_not_allowed"),
        ("https://other.example.test/", "host_not_allowed"),
        ("ftp://hook.example.test/", "invalid_url"),
    ] {
        let (status, v) = e.call("POST", ENDPOINTS, Some(json!({ "key": "x", "name": "X", "url": url }))).await;
        assert_eq!((status, details(&v)), (400, vec![("url".into(), reason.into())]), "{url}: {v}");
    }
    let reserved = json!({ "key": "x", "name": "X", "url": "https://hook.example.test/",
        "authHeader": { "name": "X-ShadouCMDB-Signature", "value": "v" } });
    let (status, v) = e.call("POST", ENDPOINTS, Some(reserved)).await;
    assert_eq!((status, details(&v)), (400, vec![("authHeader.name".into(), "reserved".into())]), "{v}");

    let (id, secret) = e
        .endpoint(
            "itsm",
            "https://HOOK.example.test/hook",
            json!({ "authHeader": { "name": "Authorization", "value": HEADER_VALUE } }),
        )
        .await;
    assert!(secret.starts_with("whsec_"), "{secret}");
    let (status, v) = e
        .call("POST", ENDPOINTS, Some(json!({ "key": "itsm", "name": "Again", "url": "https://hook.example.test/" })))
        .await;
    assert_eq!((status, code(&v)), (409, "CONFLICT"), "{v}");

    // What the API shows from now on.
    let one = w.ok("GET", &format!("{ENDPOINTS}/{id}"), json!(null)).await;
    assert_eq!(one["url"], "https://hook.example.test/hook");
    assert_eq!((one["authHeaderName"].as_str(), one["authHeaderSet"].as_bool()), (Some("Authorization"), Some(true)));
    assert_eq!(one["status"], "active");
    let list = w.ok("GET", ENDPOINTS, json!(null)).await;
    assert_eq!(list["page"]["total"], 1);
    let renamed = w
        .ok("PATCH", &format!("{ENDPOINTS}/{id}"), json!({ "version": one["version"], "name": "ITSM production" }))
        .await;
    assert_eq!(renamed["version"], one["version"].as_i64().unwrap() + 1);
    let (status, v) = e.call("PATCH", &format!("{ENDPOINTS}/{id}"), Some(json!({ "version": 1, "name": "x" }))).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"), "{v}");
    let pong = e.ping(id).await;
    for shown in [&one, &list, &renamed, &pong] {
        let text = shown.to_string();
        assert!(!text.contains(&secret) && !text.contains(HEADER_VALUE), "{text}");
    }

    // A workflow designer sees key, name and status only, and cannot create one.
    let designers = w.profile("Designers", &[(w.server, true)]).await;
    sqlx::query(
        "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'workflows.manage')",
    )
    .bind(designers)
    .execute(&w.pool)
    .await
    .unwrap();
    let (designer, _) = w.user("designer", &[designers]).await;
    let (status, v) = w.call(&designer, "GET", ENDPOINTS, None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!((v["data"][0]["key"].as_str(), v["data"][0]["url"].as_str()), (Some("itsm"), None), "{v}");
    let (status, v) = w
        .call(
            &designer,
            "POST",
            ENDPOINTS,
            Some(json!({ "key": "y", "name": "Y", "url": "https://hook.example.test/" })),
        )
        .await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    let (nobody, _) = w.user("nobody", &[]).await;
    let (status, _) = w.call(&nobody, "GET", ENDPOINTS, None).await;
    assert_eq!(status, 403);

    // The audit rows: the configuration, never a secret.
    let rows: Vec<(String, Option<Value>, Option<Value>)> = sqlx::query_as(
        "SELECT action, old_value, new_value FROM audit_log
         WHERE entity_type IN ('webhook_endpoints', 'webhook_allowed_hosts') ORDER BY id",
    )
    .fetch_all(&w.pool)
    .await
    .unwrap();
    let actions: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(actions, ["create", "create", "update"]);
    assert_eq!(rows[1].2.as_ref().unwrap()["authHeaderSet"], true);
    let all = format!("{rows:?}");
    assert!(!all.contains(&secret) && !all.contains(HEADER_VALUE), "{all}");

    // The export: the endpoint with authHeaderSet, never a secret; without webhooks.manage, no section.
    let (status, file, _) = call(&w.app, "GET", "/api/v1/admin/config/export", &w.admin, None).await;
    assert_eq!(status, 200, "{file}");
    assert_eq!(file["formatVersion"], 14);
    assert_eq!(file["webhookEndpoints"][0]["key"], "itsm");
    assert_eq!(file["webhookEndpoints"][0]["authHeaderSet"], true);
    assert_eq!(file["webhookAllowedHosts"][0]["hostPattern"], "hook.example.test");
    let text = file.to_string();
    assert!(!text.contains(&secret) && !text.contains(HEADER_VALUE) && !text.contains("Ciphertext"), "{text}");
    let exporters = w.profile("Exporters", &[]).await;
    sqlx::query(
        "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'config.export_import')",
    )
    .bind(exporters)
    .execute(&w.pool)
    .await
    .unwrap();
    let (exporter, _) = w.user("exporter", &[exporters]).await;
    let (status, file, _) = call(&w.app, "GET", "/api/v1/admin/config/export", &exporter, None).await;
    assert_eq!(status, 200, "{file}");
    assert!(file.get("webhookEndpoints").is_none() && file.get("webhookAllowedHosts").is_none(), "{file}");

    // The database refuses what the API refuses (GH#836).
    let pool = e.w.pool.clone();
    let refused = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            let err = sqlx::query(sql).execute(&pool).await.unwrap_err();
            err.as_database_error().unwrap().code().unwrap().into_owned()
        }
    };
    assert_eq!(
        refused(
            "INSERT INTO webhook_endpoints (key, name, url, secret_ciphertext, secret_key_id)
             VALUES ('db', 'DB', 'https://u:p@hook.example.test/', decode(repeat('ab', 60), 'hex'), 1)"
        )
        .await,
        "23514"
    );
    assert_eq!(
        refused(
            "INSERT INTO webhook_endpoints (key, name, url, secret_ciphertext, secret_key_id)
             VALUES ('db', 'DB', 'https://hook.example.test/', ''::bytea, 1)"
        )
        .await,
        "23514"
    );
    assert_eq!(
        refused(
            "INSERT INTO webhook_endpoints (key, name, url, secret_ciphertext, secret_key_id, auth_header_name,
               auth_header_ciphertext, auth_header_key_id)
             VALUES ('db', 'DB', 'https://hook.example.test/', decode(repeat('ab', 60), 'hex'), 1, 'X-Key',
               decode(repeat('ab', 28), 'hex'), 1)"
        )
        .await,
        "23514"
    );

    // Webhooks off: nothing can be created or called.
    let off_app = app_with_webhooks(e.db.pool.clone(), Arc::new(Webhooks::off(Keyring::for_tests())));
    let (status, v, _) = call(
        &off_app,
        "POST",
        ENDPOINTS,
        &w.admin,
        Some(json!({ "key": "z", "name": "Z", "url": "https://hook.example.test/" })),
    )
    .await;
    assert_eq!((status, code(&v)), (409, "WEBHOOKS_DISABLED"), "{v}");
    let (status, v, _) = call(&off_app, "POST", &format!("{ENDPOINTS}/{id}/ping"), &w.admin, None).await;
    assert_eq!((status, code(&v)), (409, "WEBHOOKS_DISABLED"), "{v}");

    // Nothing above logged a secret or the header value.
    let logged = logs.lines().join("\n");
    assert!(!logged.is_empty(), "the capture works");
    assert!(!logged.contains(&secret) && !logged.contains(HEADER_VALUE), "{logged}");
    e.db.drop().await;
}

/// A delivery through the outbox: signed so that the documented recipe
/// verifies it, with the headers of §5.4, the envelope v1 with only the
/// listed fields, over TLS with the corporate CA. After a rotation both
/// secrets verify during the grace period; with no grace only the new one.
#[tokio::test]
async fn deliveries_are_signed_and_both_secrets_verify_during_the_grace_period() {
    let (ca, tls) = tls_server("hook.example.test");
    let Some(e) = env("webhooks_signed", on(&["127.0.0.0/8"]), Some(&ca)).await else { return };
    let receiver = Receiver::start(Some(tls)).await;
    e.allow("hook.example.test", Some(receiver.port), false).await;
    let url = format!("https://hook.example.test:{}/hooks/cmdb", receiver.port);
    let (id, secret) =
        e.endpoint("itsm", &url, json!({ "authHeader": { "name": "Authorization", "value": HEADER_VALUE } })).await;
    e.action("itsm", json!(["owner_team", "environment"])).await;
    let (approver, _) = e.w.user("approver", &[e.w.approvers]).await;
    let d = e.approve(&approver).await;
    assert_eq!(send_all(&e.w.pool, &e.hooks).await, 1);
    assert_eq!(delivery(&e.w.pool, d).await, ("delivered".into(), None, Some(200)));

    let hits = receiver.hits();
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    let now = chrono::Utc::now().timestamp();
    assert!(signing::verify(&secret, hit.header("x-shadoucmdb-signature"), &hit.body, now), "{hit:?}");
    assert_eq!(hit.header("x-shadoucmdb-signature").matches("v1=").count(), 1);
    assert_eq!(hit.header("x-shadoucmdb-event"), "workflow.transition");
    assert_eq!(hit.header("x-shadoucmdb-delivery"), d.to_string());
    assert_eq!(hit.header("idempotency-key"), d.to_string());
    assert_eq!(hit.header("x-shadoucmdb-webhook-id"), "itsm");
    assert_eq!(hit.header("content-type"), "application/json");
    assert_eq!(hit.header("authorization"), HEADER_VALUE);
    assert!(hit.header("user-agent").starts_with("ShadouCMDB/"), "{hit:?}");
    let body = hit.json();
    assert_eq!((body["specVersion"].as_str(), body["id"].as_str()), (Some("1"), Some(d.to_string().as_str())));
    assert_eq!(body["event"], "workflow.transition");
    assert_eq!(body["transition"]["key"], "approve");
    assert_eq!(body["transition"]["name"], "Approve");
    assert_eq!(
        (body["transition"]["from"].as_str(), body["transition"]["to"].as_str()),
        (Some("planned"), Some("approved"))
    );
    assert_eq!(body["definition"]["key"], "server_lifecycle");
    assert_eq!(body["ci"]["class"], "server");
    assert_eq!(
        body["ci"]["attributes"],
        json!({ "owner_team": "ops", "environment": "prod" }),
        "only the listed fields"
    );
    assert_eq!(body["actor"], json!({ "type": "user", "name": "approver" }), "a name, no id or address");
    assert!(body["instance"]["url"].as_str().unwrap().starts_with("https://cmdb.example.test/workflows/"));
    assert_eq!(body["action"]["key"], "sync");
    // Every top-level field the schema requires is there.
    let schema: Value = serde_json::from_str(include_str!("payload.schema.json")).unwrap();
    for key in schema["required"].as_array().unwrap() {
        assert!(body.get(key.as_str().unwrap()).is_some(), "{key} missing from {body}");
    }

    // Rotation: both secrets for the grace period.
    let v = e.w.ok("POST", &format!("{ENDPOINTS}/{id}/rotate-secret"), json!({ "graceHours": 24 })).await;
    let new = v["secret"].as_str().unwrap().to_owned();
    assert_ne!(new, secret);
    assert!(v["endpoint"]["previousSecretUntil"].is_string(), "{v}");
    assert_eq!(e.ping(id).await["ok"], true);
    let hit = receiver.hits().pop().unwrap();
    let signature = hit.header("x-shadoucmdb-signature");
    assert_eq!(signature.matches("v1=").count(), 2, "{signature}");
    assert!(signing::verify(&new, signature, &hit.body, now));
    assert!(signing::verify(&secret, signature, &hit.body, now));
    assert_eq!(hit.header("x-shadoucmdb-event"), "ping");
    // No grace: the old one is gone at once.
    let v = e.w.ok("POST", &format!("{ENDPOINTS}/{id}/rotate-secret"), json!({ "graceHours": 0 })).await;
    let newest = v["secret"].as_str().unwrap().to_owned();
    assert_eq!(e.ping(id).await["ok"], true);
    let hit = receiver.hits().pop().unwrap();
    let signature = hit.header("x-shadoucmdb-signature");
    assert_eq!(signature.matches("v1=").count(), 1, "{signature}");
    assert!(signing::verify(&newest, signature, &hit.body, now));
    assert!(!signing::verify(&new, signature, &hit.body, now));
    let audited: i64 = count(
        &e.w.pool,
        "SELECT count(*) FROM audit_log WHERE action = 'webhook_endpoint.rotate_secret'
           AND new_value::text NOT LIKE '%whsec_%'",
    )
    .await;
    assert_eq!(audited, 2);
    e.db.drop().await;
}

/// §5.3 at send time: each blocked range, the IPv4 carriers in IPv6
/// (`::ffff:127.0.0.1`, NAT64 `64:ff9b::a9fe:a9fe`), a name resolving to
/// `10.0.0.1` (also through the outbox: dead, audited), metadata addresses
/// inside an allowed CIDR, a rebinding name, a 302 that is not followed, and
/// http only with both switches.
#[tokio::test]
async fn ssrf_vectors_are_refused_at_send_time() {
    let Some(e) = env("webhooks_ssrf", on(&[]), None).await else { return };
    let receiver = Receiver::start(None).await;
    let pool = &e.w.pool;
    e.allow("*.example.test", None, false).await;

    // One name per blocked range of §5.3; every one is refused before anything is sent.
    let blocked = [
        "0.0.0.1",
        "10.0.0.1",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.1.1",
        "172.16.0.1",
        "192.0.0.1",
        "192.0.2.1",
        "192.168.0.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "fc00::1",
        "fe80::1",
        "ff02::1",
        "2001:db8::1",
        "::ffff:127.0.0.1",
        "64:ff9b::a9fe:a9fe",
        "2002:a00:1::1",
    ];
    for (i, ip) in blocked.iter().enumerate() {
        let host = format!("b{i}.example.test");
        e.resolver.set(&host, &[ip]);
        let (id, _) = e.endpoint(&format!("b{i}"), &format!("https://{host}:{}/", receiver.port), json!({})).await;
        let v = e.ping(id).await;
        let expected = format!("address_blocked:{}", ip.parse::<std::net::IpAddr>().unwrap());
        assert_eq!((v["ok"].as_bool(), v["reason"].as_str()), (Some(false), Some(expected.as_str())), "{ip}: {v}");
    }
    // The literal forms, allowlisted literally.
    for (n, literal) in [("l1", "[::ffff:127.0.0.1]"), ("l2", "[64:ff9b::a9fe:a9fe]")] {
        e.allow(literal, None, false).await;
        let (id, _) = e.endpoint(n, &format!("https://{literal}/"), json!({})).await;
        let v = e.ping(id).await;
        assert!(v["reason"].as_str().unwrap().starts_with("address_blocked:"), "{literal}: {v}");
    }
    let v = e.ping(e.endpoint("l3", "https://[64:ff9b::a9fe:a9fe]:8443/", json!({})).await.0).await;
    assert!(v["message"].as_str().unwrap().contains("cloud metadata service (169.254.169.254)"), "{v}");
    assert!(receiver.hits().is_empty(), "nothing reached a receiver");

    // Through the outbox: a name that resolves to 10.0.0.1 is dead at once, audited.
    e.resolver.set("ten.example.test", &["10.0.0.1"]);
    e.endpoint("itsm", "https://ten.example.test/hook", json!({})).await;
    e.action("itsm", json!([])).await;
    let (approver, _) = e.w.user("approver", &[e.w.approvers]).await;
    let d = e.approve(&approver).await;
    assert_eq!(send_all(pool, &e.hooks).await, 1);
    assert_eq!(delivery(pool, d).await, ("dead".into(), Some("address_blocked:10.0.0.1".into()), None));
    let dead = count(
        pool,
        &format!(
            "SELECT count(*) FROM audit_log WHERE action = 'workflow.action_dead' AND entity_id = '{d}'
               AND new_value->>'reason' = 'address_blocked:10.0.0.1'"
        ),
    )
    .await;
    assert_eq!(dead, 1);

    // Metadata stays refused inside an allowed private range; its neighbours do not.
    let wide = hooks(on(&["169.254.0.0/16", "100.64.0.0/10", "fd00::/8", "127.0.0.0/8"]), &e.resolver, None);
    for (host, ip) in [
        ("m1.example.test", "169.254.169.254"),
        ("m2.example.test", "169.254.170.2"),
        ("m3.example.test", "fd00:ec2::254"),
        ("m4.example.test", "100.100.100.200"),
    ] {
        e.resolver.set(host, &[ip]);
        let (id, _) = e.endpoint(&host[..2], &format!("https://{host}/"), json!({})).await;
        let v = channel::ping(pool, &wide, id).await.unwrap();
        assert_eq!(v.reason.as_deref(), Some(format!("address_blocked:{ip}").as_str()), "{host}");
        assert!(v.message.contains("cloud metadata"), "{}", v.message);
    }

    // DNS rebinding: public enough at the first check, private at the next.
    // The first request reaches the receiver through the pinned address
    // (the system resolver does not know the name at all).
    let pinned = hooks(WebhooksConfig { allow_http: true, ..on(&["127.0.0.0/8"]) }, &e.resolver, None);
    e.resolver.set("rebind.example.test", &["127.0.0.1"]);
    sqlx::query(
        "INSERT INTO webhook_allowed_hosts (host_pattern, allow_http, created_by_name)
         VALUES ('rebind.example.test', true, 'test')",
    )
    .execute(pool)
    .await
    .unwrap();
    let rebind = Uuid::new_v4();
    insert_endpoint(pool, rebind, "rebind", &format!("http://rebind.example.test:{}/", receiver.port)).await;
    let v = channel::ping(pool, &pinned, rebind).await.unwrap();
    assert!(v.ok, "{v:?}");
    assert_eq!(receiver.hits().len(), 1);
    e.resolver.set("rebind.example.test", &["10.0.0.1"]);
    let v = channel::ping(pool, &pinned, rebind).await.unwrap();
    assert_eq!(v.reason.as_deref(), Some("address_blocked:10.0.0.1"), "{v:?}");
    assert_eq!(receiver.hits().len(), 1, "nothing sent after the name moved");

    // A 302 is a permanent failure; the target is never asked.
    e.resolver.set("rebind.example.test", &["127.0.0.1"]);
    let elsewhere = Receiver::start(None).await;
    receiver.answer(302, vec![("location", format!("http://127.0.0.1:{}/inside", elsewhere.port))]);
    let v = channel::ping(pool, &pinned, rebind).await.unwrap();
    assert_eq!((v.status_code, v.reason.as_deref()), (Some(302), Some("redirect_not_followed")), "{v:?}");
    assert!(v.message.contains("/inside") && v.message.contains("not followed"), "{}", v.message);
    assert!(elsewhere.hits().is_empty());
    receiver.answer(200, Vec::new());

    // http needs both switches: the operator's and the entry's.
    let strict = hooks(on(&["127.0.0.0/8"]), &e.resolver, None);
    let v = channel::ping(pool, &strict, rebind).await.unwrap();
    assert_eq!(v.reason.as_deref(), Some("http_not_allowed"), "operator switch off: {v:?}");
    sqlx::query("UPDATE webhook_allowed_hosts SET allow_http = false WHERE host_pattern = 'rebind.example.test'")
        .execute(pool)
        .await
        .unwrap();
    let v = channel::ping(pool, &pinned, rebind).await.unwrap();
    assert_eq!(v.reason.as_deref(), Some("http_not_allowed"), "entry switch off: {v:?}");
    sqlx::query("UPDATE webhook_allowed_hosts SET allow_http = true WHERE host_pattern = 'rebind.example.test'")
        .execute(pool)
        .await
        .unwrap();
    assert!(channel::ping(pool, &pinned, rebind).await.unwrap().ok, "both on");
    e.db.drop().await;
}

/// An endpoint row as the API writes it (a sealed secret), for tests that use
/// settings the test app does not have.
async fn insert_endpoint(pool: &PgPool, id: Uuid, key: &str, url: &str) {
    let sealed = crate::secrets::sealed::seal_endpoint_secret(
        &Keyring::for_tests(),
        id,
        crate::secrets::sealed::EndpointSecret::Secret,
        &signing::generate(),
    );
    sqlx::query(
        "INSERT INTO webhook_endpoints (id, key, name, url, secret_ciphertext, secret_key_id)
         VALUES ($1, $2, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(key)
    .bind(url)
    .bind(&sealed.bytes)
    .bind(sealed.key_id.0)
    .execute(pool)
    .await
    .unwrap();
}

/// The configured proxy carries every request (CONNECT, with its user name
/// and the password from WEBHOOK_PROXY_PASSWORD_FILE); the target name is
/// still vetted first, and one that resolves privately reaches nobody.
#[tokio::test]
async fn requests_go_through_the_configured_proxy() {
    let (ca, tls) = tls_server("hook.example.test");
    let Some(e) = env("webhooks_proxy", on(&[]), Some(&ca)).await else { return };
    let receiver = Receiver::start(Some(tls)).await;
    let (proxy_port, seen) = connect_proxy(receiver.port).await;
    e.allow("hook.example.test", None, false).await;
    e.allow("inside.example.test", None, false).await;
    // Public as far as the checks go; only the proxy connects, to the local receiver.
    e.resolver.set("hook.example.test", &["93.184.215.14"]);
    e.resolver.set("inside.example.test", &["192.168.1.10"]);
    let (id, _) = e.endpoint("itsm", "https://hook.example.test/hook", json!({})).await;
    let (inside, _) = e.endpoint("inside", "https://inside.example.test/hook", json!({})).await;
    let cfg = WebhooksConfig {
        proxy: WebhookProxy::Explicit(format!("http://cmdb@127.0.0.1:{proxy_port}").parse().unwrap()),
        ..on(&[])
    };
    let mut proxied = Webhooks::for_tests(cfg, Keyring::for_tests(), e.resolver.clone(), Some(&ca));
    proxied.proxy_password = Some("proxy-pw".into());
    let v = channel::ping(&e.w.pool, &proxied, id).await.unwrap();
    assert!(v.ok, "{v:?}");
    assert_eq!(receiver.hits().len(), 1);
    let heads = seen.lock().unwrap().clone();
    assert_eq!(heads.len(), 1, "{heads:?}");
    assert!(heads[0].starts_with("CONNECT hook.example.test:443 "), "{heads:?}");
    // base64("cmdb:proxy-pw")
    // Header names arrive lower-case from hyper.
    assert!(heads[0].contains("proxy-authorization: Basic Y21kYjpwcm94eS1wdw=="), "{heads:?}");
    let v = channel::ping(&e.w.pool, &proxied, inside).await.unwrap();
    assert_eq!(v.reason.as_deref(), Some("address_blocked:192.168.1.10"), "{v:?}");
    assert_eq!(seen.lock().unwrap().len(), 1, "the proxy was not asked");
    // NO_PROXY exempts a host from an HTTPS_PROXY taken from the environment.
    let env_proxy = WebhooksConfig {
        proxy: WebhookProxy::FromEnv {
            url: format!("http://127.0.0.1:{proxy_port}").parse().unwrap(),
            no_proxy: Some(".example.test".into()),
        },
        ..on(&["127.0.0.0/8"])
    };
    e.resolver.set("hook.example.test", &["127.0.0.1"]);
    sqlx::query("UPDATE webhook_endpoints SET url = $2 WHERE id = $1")
        .bind(id)
        .bind(format!("https://hook.example.test:{}/hook", receiver.port))
        .execute(&e.w.pool)
        .await
        .unwrap();
    let direct = Webhooks::for_tests(env_proxy, Keyring::for_tests(), e.resolver.clone(), Some(&ca));
    assert!(channel::ping(&e.w.pool, &direct, id).await.unwrap().ok);
    assert_eq!(seen.lock().unwrap().len(), 1, "NO_PROXY: direct");
    e.db.drop().await;
}

/// 20 failures in a row with no success for 15 minutes suspend the endpoint:
/// one `webhook_endpoint.suspend` row (actor system), the inbox notice to the
/// `webhooks.manage` holders, its waiting deliveries held; resume releases them.
#[tokio::test]
async fn twenty_failures_suspend_the_endpoint_with_an_audit_row_and_an_inbox_notice() {
    let (logs, _guard) = crate::auth::setup_token::capture::warnings();
    let Some(e) = env("webhooks_breaker", on(&["127.0.0.0/8"]), None).await else { return };
    let receiver = Receiver::start(None).await;
    receiver.answer(503, vec![("retry-after", "120".into())]);
    let mut cfg = on(&["127.0.0.0/8"]);
    cfg.allow_http = true;
    let hooks = hooks(cfg, &e.resolver, None);
    sqlx::query(
        "INSERT INTO webhook_allowed_hosts (host_pattern, allow_http, created_by_name)
         VALUES ('hook.example.test', true, 'test')",
    )
    .execute(&e.w.pool)
    .await
    .unwrap();
    let id = Uuid::new_v4();
    insert_endpoint(&e.w.pool, id, "itsm", &format!("http://hook.example.test:{}/", receiver.port)).await;
    e.action("itsm", json!([])).await;
    let (approver, _) = e.w.user("approver", &[e.w.approvers]).await;
    let first = e.approve(&approver).await;
    let second = e.approve(&approver).await;

    // The first attempt: 503 with Retry-After is transient, retried in 120 s.
    let channel = channel::channel(e.w.pool.clone(), hooks.clone());
    let claimed =
        outbox::claim_deliveries(&e.w.pool, "t", WorkflowActionKind::Webhook, channel.timeout, 1).await.unwrap();
    assert_eq!(claimed.len(), 1);
    let outcome = (channel.send)(claimed[0].clone()).await;
    assert!(
        matches!(&outcome, outbox::Outcome::Transient { status_code: Some(503), retry_after: Some(d), .. }
        if *d == Duration::from_secs(120)),
        "{outcome:?}"
    );
    outbox::record(&e.w.pool, &WorkflowActionsConfig::default(), &claimed[0], outcome).await.unwrap();
    let wait: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM next_attempt_at - now())::float8 FROM workflow_action_deliveries WHERE id = $1",
    )
    .bind(claimed[0].id)
    .fetch_one(&e.w.pool)
    .await
    .unwrap();
    assert!((100.0..=121.0).contains(&wait), "{wait}");

    // 19 more failed attempts (as retries of the same delivery would make).
    let again = Claimed { attempts: 2, ..claimed[0].clone() };
    for n in 2..=20 {
        let status: String = sqlx::query_scalar("SELECT status FROM webhook_endpoints WHERE id = $1")
            .bind(id)
            .fetch_one(&e.w.pool)
            .await
            .unwrap();
        assert_eq!(status, "active", "before failure {n}");
        let _ = (channel.send)(again.clone()).await;
    }
    let (status, reason, failures): (String, Option<String>, i32) =
        sqlx::query_as("SELECT status, suspended_reason, consecutive_failures FROM webhook_endpoints WHERE id = $1")
            .bind(id)
            .fetch_one(&e.w.pool)
            .await
            .unwrap();
    assert_eq!(
        (status.as_str(), reason.as_deref(), failures),
        ("suspended", Some("breaker"), 20),
        "{:?}",
        logs.lines()
    );
    let audit: Vec<(String, Value)> = sqlx::query_as(
        "SELECT actor_type, new_value FROM audit_log WHERE action = 'webhook_endpoint.suspend' AND entity_id = $1",
    )
    .bind(id)
    .fetch_all(&e.w.pool)
    .await
    .unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].0, "system");
    assert_eq!(
        (audit[0].1["reason"].as_str(), audit[0].1["consecutiveFailures"].as_i64()),
        (Some("breaker"), Some(20))
    );
    let notice: Vec<(String, Value)> = sqlx::query_as(
        "SELECT u.username, n.data FROM notifications n JOIN users u ON u.id = n.user_id
         WHERE n.kind = 'webhook_suspended' AND n.entity_id = $1",
    )
    .bind(id)
    .fetch_all(&e.w.pool)
    .await
    .unwrap();
    assert_eq!(notice.iter().map(|n| n.0.as_str()).collect::<Vec<_>>(), ["admin"], "webhooks.manage holders only");
    assert_eq!(notice[0].1["endpointKey"], "itsm");
    let (status, v) = e.w.call(&e.w.admin, "GET", "/api/v1/notifications?kind=webhook_suspended", None).await;
    assert_eq!((status, v["data"][0]["entityType"].as_str()), (200, Some("webhook_endpoints")), "{v}");
    assert_eq!(delivery(&e.w.pool, second).await.0, "held", "its waiting delivery is held");
    // A held delivery is not claimed; a further failure audits nothing more.
    assert_eq!(send_all(&e.w.pool, &hooks).await, 0);

    // Resume: active, the count starts again, held deliveries go out.
    let api_hooks = e.hooks.clone();
    assert!(api_hooks.cfg.allowed);
    receiver.answer(200, Vec::new());
    sqlx::query("UPDATE workflow_action_deliveries SET next_attempt_at = now() WHERE id = $1")
        .bind(first)
        .execute(&e.w.pool)
        .await
        .unwrap();
    let (status, v) = e.call("POST", &format!("{ENDPOINTS}/{id}/resume"), None).await;
    assert_eq!(status, 400, "the test API's settings do not allow http: {v}");
    let resumed = super::service::resume(&e.w.pool, &RequestContext::system("test", "test"), &hooks, id).await.unwrap();
    assert_eq!(
        (resumed.status, resumed.consecutive_failures),
        (super::service::WebhookEndpointStatus::Active, Some(0))
    );
    assert_eq!(send_all(&e.w.pool, &hooks).await, 2);
    assert_eq!(delivery(&e.w.pool, second).await.0, "delivered");
    assert_eq!(delivery(&e.w.pool, first).await.0, "delivered");
    e.db.drop().await;
}

/// The claim keeps an endpoint's `maxInFlight` and `maxPerMinute`, across
/// claims; a paused endpoint's deliveries are held, not claimed.
#[tokio::test]
async fn the_claim_keeps_each_endpoints_limits() {
    let Some(e) = env("webhooks_claim", on(&["127.0.0.0/8"]), None).await else { return };
    let pool = &e.w.pool;
    let id = Uuid::new_v4();
    insert_endpoint(pool, id, "itsm", "https://hook.example.test/").await;
    sqlx::query("UPDATE webhook_endpoints SET max_in_flight = 2, max_per_minute = 3 WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    for n in 0..6 {
        let run: i64 = sqlx::query_scalar(
            "INSERT INTO workflow_action_runs (event_id, action_key, kind, definition_id, instance_id, ci_id, status)
             VALUES ($1, 'sync', 'webhook', gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 'fanned_out')
             RETURNING id",
        )
        .bind(1_000_000 + n)
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO workflow_action_deliveries (run_id, recipient_key, endpoint_id, status)
             VALUES ($1, $2, $3, 'pending')",
        )
        .bind(run)
        .bind(format!("endpoint:{id}"))
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }
    let claim = || outbox::claim_deliveries(pool, "t", WorkflowActionKind::Webhook, Duration::from_secs(40), 10);
    let first = claim().await.unwrap();
    assert_eq!(first.len(), 2, "maxInFlight");
    assert_eq!(claim().await.unwrap().len(), 0, "still two in flight");
    for c in &first {
        outbox::record(
            pool,
            &WorkflowActionsConfig::default(),
            c,
            outbox::Outcome::Delivered { status_code: Some(200) },
        )
        .await
        .unwrap();
    }
    assert_eq!(claim().await.unwrap().len(), 1, "maxPerMinute: 3 this minute");
    assert_eq!(
        count(pool, "SELECT max(count)::bigint FROM workflow_action_rate_windows WHERE scope LIKE 'endpoint:%'").await,
        3
    );
    sqlx::query("DELETE FROM workflow_action_rate_windows").execute(pool).await.unwrap();
    super::service::pause(pool, &RequestContext::system("test", "test"), id).await.unwrap();
    assert_eq!(claim().await.unwrap().len(), 0, "paused");
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_deliveries WHERE status = 'held'").await, 3);
    e.db.drop().await;
}

/// A key rotation (ENCRYPTION_KEY_PREVIOUS_FILE) re-encrypts all three sealed
/// columns at start-up; the secret the receiver has keeps verifying. A key
/// that is lost stops the server until `webhooks reset-undecryptable`
/// suspends the endpoint (GH#836).
#[tokio::test]
async fn a_key_rotation_re_encrypts_the_endpoint_secrets() {
    use crate::secrets::sealed::{self, SealedTable};
    use crate::secrets::{KeyId, new_key};
    let Some(db) = scratch::database("webhooks_key_rotation").await else { return };
    let (old, new) = (new_key(), new_key());
    let ring_old = Arc::new(Keyring::from_keys(&old, None));
    let resolver = Arc::new(StaticResolver::default());
    let w = Webhooks::for_tests(on(&[]), ring_old.clone(), resolver.clone(), None);
    sqlx::query("INSERT INTO webhook_allowed_hosts (host_pattern, created_by_name) VALUES ('hook.example.test', 't')")
        .execute(&db.pool)
        .await
        .unwrap();
    let system = RequestContext::system("test", "test");
    let body: super::service::WebhookEndpointCreate = serde_json::from_value(json!({
        "key": "itsm", "name": "ITSM", "url": "https://hook.example.test/",
        "authHeader": { "name": "X-Api-Key", "value": "key-123" } }))
    .unwrap();
    let created = super::service::create(&db.pool, &system, &w, &body).await.unwrap();
    let id = created.endpoint.id;
    let rotated = super::service::rotate(&db.pool, &system, &w, id, &Default::default()).await.unwrap();
    let key_ids = || async {
        sqlx::query_as::<_, (i32, Option<i32>, Option<i32>)>(
            "SELECT secret_key_id, previous_secret_key_id, auth_header_key_id FROM webhook_endpoints WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap()
    };
    let a = ring_old.active_id().0;
    assert_eq!(key_ids().await, (a, Some(a), Some(a)));
    let counts = sealed::key_counts(&mut db.pool.acquire().await.unwrap()).await.unwrap();
    assert!(counts.contains(&sealed::KeyCount { table: SealedTable::WebhookEndpoints, key_id: KeyId(a), rows: 3 }));

    // Start-up with the new key and the old one as previous: everything moves to the new key.
    let ring = Arc::new(Keyring::from_keys(&new, Some(&old)));
    let done = sealed::prepare(&db.pool, &ring).await.unwrap();
    let mine = done.iter().find(|d| d.table == SealedTable::WebhookEndpoints).unwrap();
    assert_eq!((mine.from_previous, mine.failed), (1, 0));
    let b = ring.active_id().0;
    assert_eq!(key_ids().await, (b, Some(b), Some(b)));
    // Readable with the new key alone, and the receiver's secrets still verify.
    let only_new = Arc::new(Keyring::from_keys(&new, None));
    let w = Webhooks::for_tests(on(&[]), only_new.clone(), resolver.clone(), None);
    let row = super::service::row(&mut db.pool.acquire().await.unwrap(), id, false).await.unwrap();
    let secrets = row.signing_secrets(&w).unwrap();
    assert_eq!(secrets.len(), 2);
    let header = signing::header(1, b"x", &[&secrets[0][..], &secrets[1][..]]);
    assert!(signing::verify(&rotated.secret, &header, b"x", 1));
    assert!(signing::verify(&created.secret, &header, b"x", 1));
    assert_eq!(&row.auth_header(&w).unwrap().unwrap().1[..], b"key-123");

    // A lost key: the server refuses to start and names the command that helps.
    let lost = Keyring::random();
    let counts = sealed::key_counts(&mut db.pool.acquire().await.unwrap()).await.unwrap();
    let message = sealed::refusal(&counts, lost.active_id(), None).unwrap();
    assert!(
        message.contains("3 webhook endpoint secrets") && message.contains("webhooks reset-undecryptable"),
        "{message}"
    );
    let args = crate::secrets::cli::ResetUndecryptableArgs {
        dry_run: false,
        no_key: false,
        confirm: crate::maintenance::ConfirmArgs { yes: true },
    };
    crate::secrets::cli::reset_endpoints(&db.pool, Some(&lost), Some((lost.active_id(), None)), &args).await.unwrap();
    let (status, reason, prev, header): (String, Option<String>, Option<Vec<u8>>, Option<Vec<u8>>) = sqlx::query_as(
        "SELECT status, suspended_reason, previous_secret_ciphertext, auth_header_ciphertext FROM webhook_endpoints",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!((status.as_str(), reason.as_deref(), prev, header), ("suspended", Some("secret_required"), None, None));
    let counts = sealed::key_counts(&mut db.pool.acquire().await.unwrap()).await.unwrap();
    assert_eq!(sealed::refusal(&counts, lost.active_id(), None), None, "the server starts again");
    db.drop().await;
}

/// Format 14: an import creates a new endpoint suspended (`secret_required`)
/// with a secret nobody has seen; an existing one keeps its secret; resume
/// needs a rotation first; the sections need `webhooks.manage`.
#[tokio::test]
async fn an_import_creates_new_endpoints_suspended_until_their_secret_is_rotated() {
    let Some(e) = env("webhooks_import", on(&["127.0.0.0/8"]), None).await else { return };
    let w = &e.w;
    e.allow("hook.example.test", None, false).await;
    let (id, _) = e.endpoint("itsm", "https://hook.example.test/a", json!({})).await;
    let secret_before: Vec<u8> = sqlx::query_scalar("SELECT secret_ciphertext FROM webhook_endpoints WHERE id = $1")
        .bind(id)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    let (_, mut file, _) = call(&w.app, "GET", "/api/v1/admin/config/export", &w.admin, None).await;
    file["webhookEndpoints"][0]["name"] = json!("ITSM renamed");
    file["webhookEndpoints"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "key": "crm", "name": "CRM", "url": "https://crm.example.test/hook", "authHeaderSet": true }));
    file["webhookAllowedHosts"].as_array_mut().unwrap().push(json!({ "hostPattern": "*.crm.example.test" }));

    let (status, v, _) =
        call(&w.app, "POST", "/api/v1/admin/config/import?mode=apply", &w.admin, Some(file.clone())).await;
    assert_eq!(status, 200, "{v}");
    let warnings = v["warnings"].to_string();
    assert!(warnings.contains("Endpoint crm is created suspended"), "{warnings}");
    assert!(warnings.contains("auth header") && warnings.contains("not on the webhook host allowlist"), "{warnings}");
    let rows: Vec<(String, String, String, Option<String>, Vec<u8>)> = sqlx::query_as(
        "SELECT key, name, status, suspended_reason, secret_ciphertext FROM webhook_endpoints ORDER BY key",
    )
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(
        (rows[0].0.as_str(), rows[0].2.as_str(), rows[0].3.as_deref()),
        ("crm", "suspended", Some("secret_required"))
    );
    assert_eq!((rows[1].1.as_str(), rows[1].2.as_str()), ("ITSM renamed", "active"));
    assert_eq!(rows[1].4, secret_before, "an existing endpoint keeps its secret");
    assert_eq!(count(&w.pool, "SELECT count(*) FROM webhook_allowed_hosts").await, 2);

    // Resume asks for a rotation first; after it the endpoint is paused, then resumes.
    let crm: Uuid =
        sqlx::query_scalar("SELECT id FROM webhook_endpoints WHERE key = 'crm'").fetch_one(&w.pool).await.unwrap();
    e.allow("crm.example.test", None, false).await;
    let (status, v) = e.call("POST", &format!("{ENDPOINTS}/{crm}/resume"), None).await;
    assert_eq!((status, code(&v)), (422, "SECRET_REQUIRED"), "{v}");
    let v = w.ok("POST", &format!("{ENDPOINTS}/{crm}/rotate-secret"), json!({})).await;
    assert_eq!(v["endpoint"]["status"], "paused");
    let v = w.ok("POST", &format!("{ENDPOINTS}/{crm}/resume"), json!(null)).await;
    assert_eq!(v["status"], "active");

    // The sections need webhooks.manage, dry run included.
    let importers = w.profile("Importers", &[]).await;
    sqlx::query(
        "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'config.export_import')",
    )
    .bind(importers)
    .execute(&w.pool)
    .await
    .unwrap();
    let (importer, _) = w.user("importer", &[importers]).await;
    let only = json!({ "format": "shadoucmdb.config", "formatVersion": 14,
        "webhookAllowedHosts": [{ "hostPattern": "evil.example.test" }] });
    let (status, v, _) = call(&w.app, "POST", "/api/v1/admin/config/import?mode=dry_run", &importer, Some(only)).await;
    assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
    // A format-13 file knows nothing of webhooks and changes none.
    let v13 = json!({ "format": "shadoucmdb.config", "formatVersion": 13 });
    let (status, v, _) = call(&w.app, "POST", "/api/v1/admin/config/import?mode=apply", &w.admin, Some(v13)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(count(&w.pool, "SELECT count(*) FROM webhook_endpoints").await, 2);
    e.db.drop().await;
}

/// Removing an allowlist entry suspends the endpoints it alone allowed, and
/// an endpoint used by an action cannot be deleted.
#[tokio::test]
async fn the_allowlist_and_actions_keep_endpoints_consistent() {
    let Some(e) = env("webhooks_allowlist", on(&["127.0.0.0/8"]), None).await else { return };
    let host = e.allow("hook.example.test", None, false).await;
    e.allow("*.other.example.test", None, false).await;
    let (id, _) = e.endpoint("itsm", "https://hook.example.test/", json!({})).await;
    let (other, _) = e.endpoint("other", "https://a.other.example.test/", json!({})).await;
    e.action("itsm", json!(["owner_team"])).await;

    // An action naming an unknown endpoint or field is refused; a paused endpoint cannot be newly chosen.
    let path = format!("{DEFS}/{}/actions", e.w.definition);
    let version = e.w.ok("GET", &path, json!(null)).await["version"].clone();
    let action = |endpoint: &str, fields: Value| {
        json!({ "version": version, "actions": [{ "key": "sync", "name": "Sync", "kind": "webhook",
            "trigger": "transition", "transition": "approve", "endpoint": endpoint,
            "settings": { "includeAttributes": fields } }] })
    };
    let (status, v) = e.call("PUT", &path, Some(action("nope", json!([])))).await;
    assert_eq!((status, details(&v)), (400, vec![("actions[0].endpoint".into(), "not_found".into())]), "{v}");
    let (status, v) = e.call("PUT", &path, Some(action("itsm", json!(["no_such_field"])))).await;
    assert_eq!(
        (status, details(&v)),
        (400, vec![("actions[0].settings.includeAttributes[0]".into(), "unknown_attribute".into())]),
        "{v}"
    );
    // Only the fields of the workflow's type: another type's field, or a column
    // name of a sealed secret (no CI field is ever sealed), is refused per key.
    e.w.ok(
        "POST",
        "/api/v1/attribute-definitions",
        json!({ "classId": e.w.network, "key": "snmp_community", "label": "SNMP community", "dataType": "text" }),
    )
    .await;
    let (status, v) = e
        .call(
            "PUT",
            &path,
            Some(action("itsm", json!(["owner_team", "snmp_community", "secret_ciphertext", "password_hash"]))),
        )
        .await;
    let refused: Vec<(String, String)> =
        (1..4).map(|i| (format!("actions[0].settings.includeAttributes[{i}]"), "unknown_attribute".into())).collect();
    assert_eq!((status, details(&v)), (400, refused), "{v}");
    e.w.ok("POST", &format!("{ENDPOINTS}/{other}/pause"), json!(null)).await;
    let (status, v) = e.call("PUT", &path, Some(action("other", json!([])))).await;
    assert_eq!((status, details(&v)), (400, vec![("actions[0].endpoint".into(), "endpoint_not_active".into())]), "{v}");

    let (status, v) = e.call("DELETE", &format!("{ENDPOINTS}/{id}"), None).await;
    assert_eq!((status, code(&v)), (409, "IN_USE"), "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("server_lifecycle.sync"), "{v}");

    let host_id = host["id"].as_str().unwrap();
    let v = e.w.ok("DELETE", &format!("{HOSTS}/{host_id}"), json!(null)).await;
    assert_eq!(v["suspendedEndpoints"], json!(["itsm"]));
    let lint = e.w.ok("GET", &path, json!(null)).await;
    assert!(lint["problems"].to_string().contains("endpoint_not_active"), "{lint}");
    let (status, v) = e.call("POST", &format!("{ENDPOINTS}/{id}/resume"), None).await;
    assert_eq!((status, details(&v)), (400, vec![("url".into(), "host_not_allowed".into())]), "{v}");
    let reasons: Vec<String> = sqlx::query_scalar(
        "SELECT new_value->>'reason' FROM audit_log WHERE action = 'webhook_endpoint.suspend' ORDER BY id",
    )
    .fetch_all(&e.w.pool)
    .await
    .unwrap();
    assert_eq!(reasons, ["paused", "host_not_allowed"]);
    e.db.drop().await;
}

/// A receiver that repeats the request in its error answer: the stored excerpt
/// (`last_error`), the audit rows and the log hold no payload value, no
/// signature, no secret and no header value (Mamori, SHAA-2857).
#[tokio::test]
async fn an_echoing_receiver_puts_no_payload_or_secret_in_the_error_audit_or_log() {
    let (ca, tls) = tls_server("hook.example.test");
    let Some(e) = env("webhooks_echo", on(&["127.0.0.0/8"]), Some(&ca)).await else { return };
    let (logs, _guard) = crate::auth::setup_token::capture::json();
    let receiver = Receiver::start(Some(tls)).await;
    e.allow("hook.example.test", Some(receiver.port), false).await;
    let url = format!("https://hook.example.test:{}/hooks/cmdb", receiver.port);
    let (_, secret) =
        e.endpoint("itsm", &url, json!({ "authHeader": { "name": "Authorization", "value": HEADER_VALUE } })).await;
    e.action("itsm", json!(["owner_team", "environment"])).await;
    let (approver, _) = e.w.user("approver", &[e.w.approvers]).await;

    // The whole request back (400: dead at once), then a part of it (a field value, 422).
    let replies = [
        (ECHO.to_owned(), 400),
        ("{\"error\":\"environment prod is not accepted here\"}".to_owned(), 422),
        ("{\"error\":\"unknown change ticket\"}".to_owned(), 422),
    ];
    let mut stored = Vec::new();
    for (reply, status) in replies {
        *receiver.reply.lock().unwrap() = Some(reply);
        receiver.answer(status, Vec::new());
        let d = e.approve(&approver).await;
        assert_eq!(send_all(&e.w.pool, &e.hooks).await, 1);
        let (state, reason, code): (String, Option<String>, Option<i32>) = delivery(&e.w.pool, d).await;
        assert_eq!((state.as_str(), reason.as_deref(), code), ("dead", Some("http_status"), Some(i32::from(status))));
        let error: String = sqlx::query_scalar("SELECT last_error FROM workflow_action_deliveries WHERE id = $1")
            .bind(d)
            .fetch_one(&e.w.pool)
            .await
            .unwrap();
        stored.push(error);
    }
    let hit = receiver.hits().into_iter().next().unwrap();
    let signature = hit.header("x-shadoucmdb-signature").to_owned();
    let mac = signature.split("v1=").nth(1).unwrap().to_owned();
    assert_eq!(stored[0], "HTTP 400: (response body withheld: it repeats part of the request)");
    assert_eq!(stored[1], "HTTP 422: (response body withheld: it repeats part of the request)");
    assert_eq!(stored[2], "HTTP 422: {\"error\":\"unknown change ticket\"}", "an unrelated answer is kept");

    let audit: Vec<String> = sqlx::query_scalar(
        "SELECT concat_ws(' ', old_value::text, new_value::text) FROM audit_log
          WHERE action LIKE 'webhook%' OR action LIKE 'workflow.action%'",
    )
    .fetch_all(&e.w.pool)
    .await
    .unwrap();
    assert!(!audit.is_empty());
    let logged = logs.lines().join("\n");
    let body = String::from_utf8_lossy(&hit.body).into_owned();
    for (place, text) in [("last_error", stored.join("\n")), ("audit", audit.join("\n")), ("log", logged)] {
        for needle in [&secret, HEADER_VALUE, &mac, &body, "\"environment\":\"prod\"", "\"owner_team\":\"ops\""] {
            assert!(!text.contains(needle), "{place} holds {needle}: {text}");
        }
    }
    e.db.drop().await;
}
