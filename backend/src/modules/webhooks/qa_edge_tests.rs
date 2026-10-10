//! QA edge tests (SHAA-2979): S5 webhooks (PR #853) at their edges. The
//! signature header as a receiver checks it (format, the documented HMAC
//! input, a current timestamp); a receiver that never answers (the endpoint's
//! timeout, then a retry); every 3xx left unfollowed; endless or oversized
//! answers cut off; and the SSRF refusals for the IPv4 ranges, the literal
//! spellings of an IPv4 address and the IPv6 forms main handles.
//!
//! Most tests drive [`Webhooks::send`] directly against local listeners: the
//! test hook for loopback is `WEBHOOK_ALLOW_PRIVATE_CIDRS` (`allow_private`),
//! left empty where the refusal itself is under test. Those need no database;
//! the ping and outbox tests do and are skipped without one.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hmac::{Hmac, KeyInit, Mac};
use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper_util::rt::TokioIo;
use reqwest::header::HeaderMap;
use serde_json::json;
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use super::client::{Attempt, Request};
use super::ssrf::tests::StaticResolver;
use super::{Webhooks, channel, signing};
use crate::api::context::RequestContext;
use crate::config::{WebhooksConfig, WorkflowActionsConfig};
use crate::db::scratch;
use crate::modules::api_tokens::tests::app_with_webhooks;
use crate::modules::workflows::actions::WorkflowActionKind;
use crate::modules::workflows::actions::outbox;
use crate::modules::workflows::runtime_tests::{DEFS, world_with_app};
use crate::secrets::Keyring;

// ---------------------------------------------------------------------------
// Local listeners
// ---------------------------------------------------------------------------

/// A request a [`Receiver`] got.
#[derive(Debug, Clone)]
struct Hit {
    path: String,
    headers: HashMap<String, Vec<String>>,
    body: Vec<u8>,
}

impl Hit {
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).and_then(|v| v.first()).map(String::as_str).unwrap_or_default()
    }
}

/// A status and the headers to answer with.
type Answer = (u16, Vec<(&'static str, String)>);

/// A plain-http receiver: records every request, answers `status` with `headers`.
#[derive(Clone)]
struct Receiver {
    port: u16,
    hits: Arc<Mutex<Vec<Hit>>>,
    answer: Arc<Mutex<Answer>>,
}

impl Receiver {
    async fn start() -> Receiver {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let r = Receiver {
            port: listener.local_addr().unwrap().port(),
            hits: Arc::default(),
            answer: Arc::new(Mutex::new((200, Vec::new()))),
        };
        let shared = r.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let r = shared.clone();
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(move |req: hyper::Request<Incoming>| {
                        let r = r.clone();
                        async move { Ok::<_, std::convert::Infallible>(r.handle(req).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tcp), svc).await;
                });
            }
        });
        r
    }

    async fn handle(&self, req: hyper::Request<Incoming>) -> hyper::Response<Full<Bytes>> {
        let path = req.uri().path().to_owned();
        let mut headers: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in req.headers() {
            headers.entry(k.as_str().to_owned()).or_default().push(v.to_str().unwrap_or_default().to_owned());
        }
        let body = req.into_body().collect().await.map(|b| b.to_bytes().to_vec()).unwrap_or_default();
        self.hits.lock().unwrap().push(Hit { path, headers, body });
        let (status, extra) = self.answer.lock().unwrap().clone();
        let mut res = hyper::Response::builder().status(status);
        for (k, v) in extra {
            res = res.header(k, v);
        }
        res.body(Full::new(Bytes::from_static(b"{\"received\":true}"))).unwrap()
    }

    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }

    fn answer(&self, status: u16, headers: Vec<(&'static str, String)>) {
        *self.answer.lock().unwrap() = (status, headers);
    }
}

/// A listener that accepts every connection and never writes a byte: (port, connections accepted).
async fn silent() -> (u16, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((tcp, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            held.push(tcp);
        }
    });
    (port, accepted)
}

/// What a [`flood`] server managed to write before the client went away.
struct Flood {
    port: u16,
    written: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
}

/// A raw server that reads the request, writes `head`, then `piece` again and
/// again (every `pause`, if given) until the client closes the connection.
async fn flood(head: &'static [u8], piece: Vec<u8>, pause: Option<Duration>) -> Flood {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let f = Flood { port: listener.local_addr().unwrap().port(), written: Arc::default(), closed: Arc::default() };
    let (written, closed) = (f.written.clone(), f.closed.clone());
    tokio::spawn(async move {
        let Ok((mut tcp, _)) = listener.accept().await else { return };
        let mut buf = vec![0u8; 64 * 1024];
        let _ = tcp.read(&mut buf).await;
        if tcp.write_all(head).await.is_ok() {
            written.fetch_add(head.len() as u64, Ordering::SeqCst);
            loop {
                if tcp.write_all(&piece).await.is_err() {
                    break;
                }
                written.fetch_add(piece.len() as u64, Ordering::SeqCst);
                if let Some(p) = pause {
                    tokio::time::sleep(p).await;
                }
            }
        }
        closed.store(true, Ordering::SeqCst);
    });
    f
}

/// Waits up to `limit` for `flag`.
async fn eventually(flag: &AtomicBool, limit: Duration) -> bool {
    let until = Instant::now() + limit;
    while Instant::now() < until {
        if flag.load(Ordering::SeqCst) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    flag.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// Set-up
// ---------------------------------------------------------------------------

fn cfg(private: &[&str]) -> WebhooksConfig {
    WebhooksConfig {
        allowed: true,
        allow_http: true,
        allow_private: private.iter().map(|n| n.parse().unwrap()).collect(),
        ..WebhooksConfig::default()
    }
}

/// Webhooks on, http allowed, these networks opened; `hook.example.test` resolves to 127.0.0.1.
fn hooks(private: &[&str]) -> (Webhooks, Arc<StaticResolver>) {
    let resolver = Arc::new(StaticResolver::default());
    resolver.set("hook.example.test", &["127.0.0.1"]);
    (Webhooks::for_tests(cfg(private), Keyring::for_tests(), resolver.clone(), None), resolver)
}

/// One POST of `{}` to `url`.
async fn post(w: &Webhooks, url: &str, timeout: Duration) -> Attempt {
    let url = url::Url::parse(url).unwrap_or_else(|e| panic!("{url}: {e}"));
    w.send(Request { url: &url, body: b"{}".to_vec(), headers: HeaderMap::new(), timeout }).await
}

const LONG: Duration = Duration::from_secs(10);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Signature
// ---------------------------------------------------------------------------

/// A ping carries exactly one `X-ShadouCMDB-Signature: t=<unix seconds>,v1=<64
/// hex>`; `t` is now; `v1` is HMAC-SHA256 keyed with the `whsec_...` string
/// over `t + "." + raw body`, computed here independently of `signing`; and
/// the documented check refuses another body, another secret and a stale `t`.
#[tokio::test]
async fn qa_the_signature_header_is_current_and_verifies_over_t_dot_body() {
    let Some(db) = scratch::database("qa_webhooks_signature").await else { return };
    let receiver = Receiver::start().await;
    let (w, _) = hooks(&["127.0.0.0/8"]);
    sqlx::query(
        "INSERT INTO webhook_allowed_hosts (host_pattern, allow_http, created_by_name)
         VALUES ('hook.example.test', true, 'qa')",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    let body: super::service::WebhookEndpointCreate = serde_json::from_value(json!({
        "key": "itsm", "name": "ITSM", "url": format!("http://hook.example.test:{}/in", receiver.port) }))
    .unwrap();
    let created = super::service::create(&db.pool, &RequestContext::system("qa", "qa"), &w, &body).await.unwrap();
    let secret = created.secret;

    let before = chrono::Utc::now().timestamp();
    let v = channel::ping(&db.pool, &w, created.endpoint.id).await.unwrap();
    let after = chrono::Utc::now().timestamp();
    assert!(v.ok, "{v:?}");
    let hits = receiver.hits();
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.path, "/in");
    assert_eq!(hit.headers["x-shadoucmdb-signature"].len(), 1, "one signature header: {hit:?}");
    let signature = hit.header("x-shadoucmdb-signature");

    // The format: t=<digits>,v1=<64 lower-case hex>, nothing else.
    let (t, mac) = signature
        .strip_prefix("t=")
        .and_then(|s| s.split_once(",v1="))
        .unwrap_or_else(|| panic!("not t=..,v1=..: {signature}"));
    assert!(!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()), "{signature}");
    assert!(mac.len() == 64 && mac.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')), "{signature}");
    let t: i64 = t.parse().unwrap();
    assert!((before..=after).contains(&t), "t={t} is not the time of sending ({before}..{after})");

    // The documented input, computed here: HMAC-SHA256(whsec_ string, t "." raw body).
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).unwrap();
    m.update(format!("{t}.").as_bytes());
    m.update(&hit.body);
    assert_eq!(mac, hex(&m.finalize().into_bytes()));
    assert!(signing::verify(&secret, signature, &hit.body, after));
    let mut tampered = hit.body.clone();
    tampered.push(b' ');
    assert!(!signing::verify(&secret, signature, &tampered, after), "another body");
    assert!(!signing::verify(&signing::display(&[9u8; 32]), signature, &hit.body, after), "another secret");
    assert!(!signing::verify(&secret, signature, &hit.body, t + signing::TOLERANCE_SECS + 1), "stale t");

    // The other headers of §5.4 name the same request.
    assert_eq!(hit.header("x-shadoucmdb-event"), "ping");
    assert_eq!(hit.header("x-shadoucmdb-webhook-id"), "itsm");
    let delivery = hit.header("x-shadoucmdb-delivery");
    assert!(delivery.parse::<Uuid>().is_ok(), "{hit:?}");
    assert_eq!(hit.header("idempotency-key"), delivery);
    assert_eq!(hit.header("content-type"), "application/json");
    let payload: serde_json::Value = serde_json::from_slice(&hit.body).unwrap();
    assert_eq!(payload["id"].as_str(), Some(delivery), "{payload}");

    // A second ping is signed afresh: a new delivery id, a new MAC.
    channel::ping(&db.pool, &w, created.endpoint.id).await.unwrap();
    let again = receiver.hits().pop().unwrap();
    assert_ne!(again.header("x-shadoucmdb-delivery"), delivery);
    assert_ne!(again.header("x-shadoucmdb-signature"), signature);
    assert!(signing::verify(&secret, again.header("x-shadoucmdb-signature"), &again.body, after + 1));
    db.drop().await;
}

// ---------------------------------------------------------------------------
// Timeouts
// ---------------------------------------------------------------------------

/// A receiver that accepts the connection and never answers: the attempt ends
/// as a transient failure at the timeout it was given, not at the 45 s of the
/// channel or never.
#[tokio::test]
async fn qa_a_receiver_that_never_answers_fails_at_the_timeout() {
    let (port, accepted) = silent().await;
    let (w, _) = hooks(&["127.0.0.0/8"]);
    let started = Instant::now();
    let a = post(&w, &format!("http://hook.example.test:{port}/"), Duration::from_millis(1000)).await;
    let took = started.elapsed();
    assert_eq!(a, Attempt::Failed { message: "No answer within 1000 ms".into() });
    assert!(took >= Duration::from_millis(900) && took < Duration::from_secs(4), "{took:?}");
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "the request did reach the listener");
}

/// Through the outbox: an endpoint with `timeoutMs` 1000 at a silent listener.
/// The delivery is not dead but pending again (a retry with backoff), with the
/// timeout as its error; the endpoint counts one failure; the ping says
/// `unreachable` within the same time.
#[tokio::test]
async fn qa_a_timed_out_delivery_is_recorded_as_a_retryable_failure() {
    let Some(db) = scratch::database("qa_webhooks_timeout").await else { return };
    let (port, _) = silent().await;
    let (w, _) = hooks(&["127.0.0.0/8"]);
    let w = Arc::new(w);
    let world = world_with_app(&db, app_with_webhooks(db.pool.clone(), w.clone())).await;
    let pool = &world.pool;
    world
        .ok(
            "POST",
            "/api/v1/admin/webhook-allowed-hosts",
            json!({ "hostPattern": "hook.example.test", "allowHttp": true }),
        )
        .await;
    let v = world
        .ok(
            "POST",
            "/api/v1/admin/webhook-endpoints",
            json!({ "key": "slow", "name": "Slow", "url": format!("http://hook.example.test:{port}/"),
                "timeoutMs": 1000 }),
        )
        .await;
    let endpoint: Uuid = v["endpoint"]["id"].as_str().unwrap().parse().unwrap();
    let path = format!("{DEFS}/{}/actions", world.definition);
    let version = world.ok("GET", &path, json!(null)).await["version"].clone();
    world
        .ok(
            "PUT",
            &path,
            json!({ "version": version, "actions": [{ "key": "sync", "name": "Sync", "kind": "webhook",
                "trigger": "transition", "transition": "approve", "endpoint": "slow" }] }),
        )
        .await;

    // Approve on a new CI, then fan the run out.
    let ci = world
        .ok(
            "POST",
            "/api/v1/configuration-items",
            json!({ "classId": world.server, "attributes": { "environment": "prod", "owner_team": "ops" } }),
        )
        .await;
    let ci: Uuid = ci["id"].as_str().unwrap().parse().unwrap();
    let (approver, _) = world.user("approver", &[world.approvers]).await;
    let (status, v) = world.start(&world.admin, ci).await;
    assert_eq!(status, 201, "{v}");
    let instance: Uuid = v["instance"]["id"].as_str().unwrap().parse().unwrap();
    let body = json!({ "transitionKey": "approve", "expectedVersion": 1,
        "fields": { "owner_team": "ops", "risk": 1 }, "comment": "CAB ok" });
    let (status, v) = world.transition(&approver, instance, body).await;
    assert_eq!(status, 200, "{v}");
    let actions_cfg = WorkflowActionsConfig::default();
    for id in outbox::claim_runs(pool, "qa", 100).await.unwrap() {
        outbox::fan_out(pool, &actions_cfg, id, "qa").await.unwrap();
    }

    let channel = channel::channel(pool.clone(), w.clone());
    let claimed = outbox::claim_deliveries(pool, "qa", WorkflowActionKind::Webhook, channel.timeout, 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    let started = Instant::now();
    let outcome = (channel.send)(claimed[0].clone()).await;
    let took = started.elapsed();
    assert!(took < Duration::from_secs(4), "the endpoint's 1 s, not the channel's 45 s: {took:?}");
    assert_eq!(
        outcome,
        outbox::Outcome::Transient { status_code: None, error: "No answer within 1000 ms".into(), retry_after: None }
    );
    assert!(outbox::record(pool, &actions_cfg, &claimed[0], outcome).await.unwrap());
    let (state, reason, attempts, error, retry_in): (String, Option<String>, i16, Option<String>, f64) =
        sqlx::query_as(
            "SELECT status, status_reason, attempts, last_error,
                    extract(epoch FROM next_attempt_at - now())::float8
             FROM workflow_action_deliveries WHERE id = $1",
        )
        .bind(claimed[0].id)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!((state.as_str(), reason, attempts), ("pending", None, 1), "retried, not dead");
    assert_eq!(error.as_deref(), Some("No answer within 1000 ms"));
    assert!(retry_in > 0.0, "a backoff before the retry: {retry_in}");
    let failures: i32 = sqlx::query_scalar("SELECT consecutive_failures FROM webhook_endpoints WHERE id = $1")
        .bind(endpoint)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(failures, 1);

    let v = channel::ping(pool, &w, endpoint).await.unwrap();
    assert_eq!((v.ok, v.reason.as_deref(), v.status_code), (false, Some("unreachable"), None), "{v:?}");
    assert!((900..4000).contains(&v.duration_ms), "{v:?}");
    db.drop().await;
}

// ---------------------------------------------------------------------------
// Redirects
// ---------------------------------------------------------------------------

/// Every redirect status, to an absolute URL on another local receiver and to
/// a relative one on the same: the answer is taken as it is (status and
/// `Location` reported), the target is never asked, nothing is asked twice.
/// 307 and 308 would repeat the signed POST with its body.
#[tokio::test]
async fn qa_no_redirect_status_is_followed() {
    let first = Receiver::start().await;
    let target = Receiver::start().await;
    let (w, _) = hooks(&["127.0.0.0/8"]);
    let url = format!("http://hook.example.test:{}/hook", first.port);
    let mut sent = 0;
    for status in [300, 301, 302, 303, 307, 308] {
        for location in [format!("http://127.0.0.1:{}/inside", target.port), "/elsewhere".to_owned()] {
            first.answer(status, vec![("location", location.clone())]);
            let a = post(&w, &url, LONG).await;
            sent += 1;
            match a {
                Attempt::Answered { status: s, location: Some(l), .. } => {
                    assert_eq!((s, l.as_str()), (status, location.as_str()));
                }
                other => panic!("{status} to {location}: {other:?}"),
            }
            assert_eq!(first.hits().len(), sent, "{status} to {location}: asked once");
        }
    }
    assert!(first.hits().iter().all(|h| h.path == "/hook"), "never /elsewhere");
    assert!(target.hits().is_empty(), "the redirect target was asked: {:?}", target.hits());
}

// ---------------------------------------------------------------------------
// Response size
// ---------------------------------------------------------------------------

/// Answers that never end: a chunked body, a body under a 10 GiB
/// Content-Length, a header that never ends. Each attempt returns long before
/// its 10 s timeout, keeps at most 1 KiB, and the connection is closed (the
/// server's next write fails), so nothing is read without bound.
#[tokio::test]
async fn qa_an_endless_answer_is_cut_off() {
    let (w, _) = hooks(&["127.0.0.0/8"]);
    let mut chunk = format!("{:x}\r\n", 16 * 1024).into_bytes();
    chunk.extend(std::iter::repeat_n(b'x', 16 * 1024));
    chunk.extend_from_slice(b"\r\n");
    let cases: [(&str, &'static [u8], Vec<u8>); 4] = [
        ("chunked 200", b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n", chunk.clone()),
        ("chunked 500", b"HTTP/1.1 500 Internal Server Error\r\nTransfer-Encoding: chunked\r\n\r\n", chunk),
        ("10 GiB", b"HTTP/1.1 200 OK\r\nContent-Length: 10737418240\r\n\r\n", vec![b'y'; 64 * 1024]),
        ("endless header", b"HTTP/1.1 200 OK\r\nX-Endless: ", vec![b'z'; 16 * 1024]),
    ];
    for (name, head, piece) in cases {
        let f = flood(head, piece, None).await;
        let started = Instant::now();
        let a = post(&w, &format!("http://hook.example.test:{}/", f.port), LONG).await;
        let took = started.elapsed();
        assert!(took < Duration::from_secs(5), "{name}: cut off by the size, not the timeout: {took:?}");
        match (&a, name) {
            (Attempt::Failed { .. }, "endless header") => {}
            (Attempt::Answered { status, excerpt, .. }, _) => {
                assert_eq!(*status, if name == "chunked 500" { 500 } else { 200 }, "{name}");
                assert!(excerpt.len() <= 1024, "{name}: {} bytes kept", excerpt.len());
            }
            _ => panic!("{name}: {a:?}"),
        }
        assert!(eventually(&f.closed, Duration::from_secs(5)).await, "{name}: the connection stays open");
        // What the kernel's socket buffers took on top of the 64 KiB read; far below "everything".
        let written = f.written.load(Ordering::SeqCst);
        assert!(written < 64 * 1024 * 1024, "{name}: {written} bytes written");
    }
}

/// A body that drips one byte every 50 ms: the attempt ends at its timeout
/// (which covers reading the body), not when the receiver stops.
#[tokio::test]
async fn qa_a_dripping_answer_ends_at_the_timeout() {
    let (w, _) = hooks(&["127.0.0.0/8"]);
    let f = flood(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
        b"1\r\nd\r\n".to_vec(),
        Some(Duration::from_millis(50)),
    )
    .await;
    let started = Instant::now();
    let a = post(&w, &format!("http://hook.example.test:{}/", f.port), Duration::from_millis(1000)).await;
    let took = started.elapsed();
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert!(matches!(&a, Attempt::Answered { status: 200, excerpt, .. } if excerpt.len() <= 1024), "{a:?}");
    assert!(eventually(&f.closed, Duration::from_secs(5)).await, "the connection stays open");
}

// ---------------------------------------------------------------------------
// SSRF
// ---------------------------------------------------------------------------

/// The address a refusal names, if `a` is one.
fn blocked(a: &Attempt) -> Option<&str> {
    match a {
        Attempt::Refused { reason, .. } => reason.strip_prefix("address_blocked:"),
        _ => None,
    }
}

/// With no private network opened, a receiver listening on 127.0.0.1 is never
/// reached: not by literal IPv4 addresses of the blocked ranges, not by the
/// other spellings of 127.0.0.1 a URL parser accepts (decimal, hex, octal,
/// short forms, `0`), not by the IPv6 forms that carry it (mapped, compatible,
/// NAT64, 6to4) or are local themselves, not by names resolving to them (also
/// when one answer of several is private), and not by `localhost` through the
/// system resolver. Opening 127.0.0.0/8 then reaches it, so the port was live.
#[tokio::test]
async fn qa_every_private_address_form_is_refused_before_sending() {
    let receiver = Receiver::start().await;
    let p = receiver.port;
    let (w, resolver) = hooks(&[]);

    // Literal hosts: (URL host, the address the refusal must name).
    let literals = [
        ("127.0.0.1", "127.0.0.1"),
        ("127.255.255.254", "127.255.255.254"),
        ("10.0.0.1", "10.0.0.1"),
        ("10.255.255.255", "10.255.255.255"),
        ("172.16.0.1", "172.16.0.1"),
        ("172.31.255.254", "172.31.255.254"),
        ("192.168.0.1", "192.168.0.1"),
        ("192.168.255.255", "192.168.255.255"),
        ("169.254.169.254", "169.254.169.254"),
        ("169.254.0.1", "169.254.0.1"),
        ("0.0.0.0", "0.0.0.0"),
        ("100.64.0.1", "100.64.0.1"),
        ("100.127.255.254", "100.127.255.254"),
        // Other spellings of 127.0.0.1 (WHATWG URL parsing).
        ("2130706433", "127.0.0.1"),
        ("0x7f000001", "127.0.0.1"),
        ("0x7f.0.0.1", "127.0.0.1"),
        ("0177.0.0.1", "127.0.0.1"),
        ("017700000001", "127.0.0.1"),
        ("127.1", "127.0.0.1"),
        ("127.0.1", "127.0.0.1"),
        ("0", "0.0.0.0"),
        // IPv6.
        ("[::1]", "::1"),
        ("[0:0:0:0:0:0:0:1]", "::1"),
        ("[::]", "::"),
        ("[fe80::1]", "fe80::1"),
        ("[febf:ffff::1]", "febf:ffff::1"),
        ("[fc00::1]", "fc00::1"),
        ("[fd12:3456:789a::1]", "fd12:3456:789a::1"),
        ("[ff02::1]", "ff02::1"),
        ("[::ffff:127.0.0.1]", "::ffff:127.0.0.1"),
        ("[::ffff:7f00:1]", "::ffff:127.0.0.1"),
        ("[::FFFF:7F00:0001]", "::ffff:127.0.0.1"),
        ("[::ffff:10.1.2.3]", "::ffff:10.1.2.3"),
        ("[::ffff:192.168.1.1]", "::ffff:192.168.1.1"),
        ("[::ffff:169.254.169.254]", "::ffff:169.254.169.254"),
        ("[::127.0.0.1]", "::127.0.0.1"),
        ("[64:ff9b::7f00:1]", "64:ff9b::7f00:1"),
        ("[2002:7f00:1::1]", "2002:7f00:1::1"),
        ("[2002:c0a8:101::1]", "2002:c0a8:101::1"),
    ];
    for (host, address) in literals {
        let a = post(&w, &format!("http://{host}:{p}/"), LONG).await;
        let expected: std::net::IpAddr = address.parse().unwrap();
        assert_eq!(blocked(&a), Some(expected.to_string().as_str()), "{host}: {a:?}");
    }

    // Names: every answer is judged, one private answer is enough.
    for (n, answers) in [
        vec!["127.0.0.1"],
        vec!["10.0.0.1"],
        vec!["172.20.0.1"],
        vec!["192.168.10.10"],
        vec!["169.254.169.254"],
        vec!["100.100.100.200"],
        vec!["0.0.0.0"],
        vec!["::1"],
        vec!["fe80::1"],
        vec!["fd00::1"],
        vec!["::ffff:127.0.0.1"],
        vec!["93.184.215.14", "127.0.0.1"],
        vec!["2606:4700::1111", "::1"],
    ]
    .iter()
    .enumerate()
    {
        let host = format!("n{n}.example.test");
        resolver.set(&host, answers);
        let a = post(&w, &format!("http://{host}:{p}/"), LONG).await;
        assert!(blocked(&a).is_some(), "{host} -> {answers:?}: {a:?}");
    }

    // localhost, as the operating system resolves it.
    let system = Webhooks::for_tests(cfg(&[]), Keyring::for_tests(), Arc::new(super::ssrf::SystemResolver), None);
    let a = post(&system, &format!("http://localhost:{p}/"), LONG).await;
    assert!(matches!(blocked(&a), Some("127.0.0.1" | "::1")), "{a:?}");
    assert!(receiver.hits().is_empty(), "a refused address was reached: {:?}", receiver.hits());

    // The control: with 127.0.0.0/8 opened, the same receiver answers.
    let (open, _) = hooks(&["127.0.0.0/8"]);
    for host in ["127.0.0.1", "2130706433", "hook.example.test"] {
        let a = post(&open, &format!("http://{host}:{p}/"), LONG).await;
        assert!(matches!(a, Attempt::Answered { status: 200, .. }), "{host}: {a:?}");
    }
    assert_eq!(receiver.hits().len(), 3);
}

/// Cloud metadata is refused whatever the operator opens (here every IPv4 and
/// IPv6 address), in each form main decodes; and opening all of IPv6 does not
/// open the IPv4 address an IPv6 one carries.
#[tokio::test]
async fn qa_metadata_and_carried_ipv4_addresses_stay_refused_when_networks_are_opened() {
    let (wide, resolver) = hooks(&["0.0.0.0/0", "::/0"]);
    for host in [
        "169.254.169.254",
        "169.254.170.2",
        "100.100.100.200",
        "[fd00:ec2::254]",
        "[::ffff:169.254.169.254]",
        "[::ffff:a9fe:a9fe]",
        "[::169.254.169.254]",
        "[64:ff9b::a9fe:a9fe]",
        "[2002:a9fe:a9fe::1]",
    ] {
        let a = post(&wide, &format!("http://{host}/latest/meta-data/"), LONG).await;
        match &a {
            Attempt::Refused { reason, message } => {
                assert!(reason.starts_with("address_blocked:"), "{host}: {a:?}");
                assert!(message.contains("cloud metadata service"), "{host}: {message}");
            }
            other => panic!("{host}: {other:?}"),
        }
    }
    resolver.set("meta.example.test", &["169.254.169.254"]);
    let a = post(&wide, "http://meta.example.test/", LONG).await;
    assert_eq!(blocked(&a), Some("169.254.169.254"), "{a:?}");

    let receiver = Receiver::start().await;
    let (v6_only, _) = hooks(&["::/0"]);
    for host in ["[::ffff:127.0.0.1]", "[::127.0.0.1]", "[64:ff9b::7f00:1]", "[2002:7f00:1::1]"] {
        let a = post(&v6_only, &format!("http://{host}:{}/", receiver.port), LONG).await;
        assert!(blocked(&a).is_some(), "{host}: {a:?}");
    }
    assert!(receiver.hits().is_empty());
}
