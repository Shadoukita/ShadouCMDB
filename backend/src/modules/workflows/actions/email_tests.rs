//! Workflow e-mail (SHAA-2734, design SHAA-2725 slice S4) on a scratch
//! database, sent to an SMTP sink in this process (no external service): who
//! gets what (view check, fixed addresses with minimal content, language),
//! redaction of what a recipient may not view, header injection through a CI
//! label, the hourly digest, one message per recipient for a bulk request,
//! and the relay's 451 (retried) and 550 (dead for that recipient only).

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use uuid::Uuid;

use super::WorkflowActionKind;
use super::outbox::{self, Channel, Channels, FanOut};
use crate::config::{MailConfig, WorkflowActionsConfig};
use crate::db::scratch;
use crate::db::upgrade_0046::{id, ok, workflow_fixture};
use crate::modules::mail::Mail;
use crate::modules::workflows::runtime_tests::{DEFS, World, world};

// ---------------------------------------------------------------------------
// The SMTP sink
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Received {
    to: Vec<String>,
    data: String,
}

#[derive(Default)]
struct SinkState {
    /// The replies to `RCPT TO` per address, in order; 250 once they run out.
    rcpt: HashMap<String, VecDeque<u16>>,
    received: Vec<Received>,
}

/// A minimal SMTP server on 127.0.0.1: no TLS, no AUTH, keeps every message.
struct Sink {
    addr: SocketAddr,
    state: Arc<Mutex<SinkState>>,
}

impl Sink {
    async fn start() -> Sink {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(SinkState::default()));
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(session(stream, shared.clone()));
            }
        });
        Sink { addr, state }
    }

    fn reply(&self, address: &str, codes: &[u16]) {
        self.state.lock().unwrap().rcpt.insert(address.to_lowercase(), codes.iter().copied().collect());
    }

    fn received(&self) -> Vec<Received> {
        self.state.lock().unwrap().received.clone()
    }

    fn to(&self, address: &str) -> Vec<Message> {
        self.received().iter().filter(|r| r.to.iter().any(|t| t.eq_ignore_ascii_case(address))).map(parse).collect()
    }
}

async fn session(stream: tokio::net::TcpStream, state: Arc<Mutex<SinkState>>) {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r);
    let _ = w.write_all(b"220 sink.test ESMTP\r\n").await;
    let mut to: Vec<String> = Vec::new();
    loop {
        let mut line = String::new();
        if lines.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let cmd = line.trim_end().to_owned();
        let upper = cmd.to_ascii_uppercase();
        let reply = if upper.starts_with("EHLO") {
            "250-sink.test\r\n250 8BITMIME\r\n".to_owned()
        } else if upper.starts_with("HELO") || upper.starts_with("NOOP") {
            "250 OK\r\n".to_owned()
        } else if upper.starts_with("MAIL FROM") || upper.starts_with("RSET") {
            to.clear();
            "250 OK\r\n".to_owned()
        } else if upper.starts_with("RCPT TO") {
            let address = cmd.split(['<', '>']).nth(1).unwrap_or_default().to_lowercase();
            let code = state.lock().unwrap().rcpt.get_mut(&address).and_then(VecDeque::pop_front).unwrap_or(250);
            if code == 250 {
                to.push(address);
                "250 OK\r\n".to_owned()
            } else {
                format!("{code} {} {address}\r\n", if code < 500 { "Try again later" } else { "No such user" })
            }
        } else if upper.starts_with("DATA") {
            let _ = w.write_all(b"354 Go ahead\r\n").await;
            let mut data = String::new();
            loop {
                let mut l = String::new();
                if lines.read_line(&mut l).await.unwrap_or(0) == 0 {
                    return;
                }
                if l == ".\r\n" {
                    break;
                }
                data.push_str(l.strip_prefix('.').filter(|_| l.starts_with("..")).unwrap_or(&l));
            }
            state.lock().unwrap().received.push(Received { to: std::mem::take(&mut to), data });
            "250 Queued\r\n".to_owned()
        } else if upper.starts_with("QUIT") {
            let _ = w.write_all(b"221 Bye\r\n").await;
            return;
        } else {
            "502 Not implemented\r\n".to_owned()
        };
        if w.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// A received message, decoded.
#[derive(Debug, Clone)]
struct Message {
    /// The raw header block.
    headers: String,
    subject: String,
    text: String,
    html: String,
}

impl Message {
    /// Everything a reader could see.
    fn all(&self) -> String {
        format!("{}\n{}\n{}", self.subject, self.text, self.html)
    }

    fn header(&self, name: &str) -> Option<String> {
        unfold(&self.headers).lines().find_map(|l| {
            l.split_once(':').filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_owned())
        })
    }
}

fn unfold(headers: &str) -> String {
    headers.replace("\r\n ", " ").replace("\r\n\t", " ").replace("\r\n", "\n")
}

fn quoted_printable(s: &str) -> Vec<u8> {
    let s = s.replace("=\r\n", "");
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'='
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn base64(s: &str) -> Vec<u8> {
    let clean: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(clean).unwrap_or_default()
}

/// RFC 2047 encoded words (`=?utf-8?b?…?=`, `=?utf-8?q?…?=`), as lettre writes them.
fn decode_words(v: &str) -> String {
    let mut out = String::new();
    let mut rest = v;
    let mut last_was_word = false;
    while !rest.is_empty() {
        match rest.find("=?") {
            Some(start) => {
                let between = &rest[..start];
                if !(last_was_word && between.trim().is_empty()) {
                    out.push_str(between);
                }
                let after = &rest[start + 2..];
                let parts: Vec<&str> = after.splitn(3, '?').collect();
                let end = parts.get(2).and_then(|p| p.find("?=")).unwrap_or(0);
                if parts.len() < 3 || end == 0 && !parts[2].starts_with("?=") {
                    out.push_str(&rest[start..]);
                    break;
                }
                let payload = &parts[2][..end];
                let bytes = if parts[1].eq_ignore_ascii_case("b") {
                    base64(payload)
                } else {
                    quoted_printable(&payload.replace('_', " "))
                };
                out.push_str(&String::from_utf8_lossy(&bytes));
                rest = &parts[2][end + 2..];
                last_was_word = true;
            }
            None => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

fn parse(r: &Received) -> Message {
    let (headers, body) = r.data.split_once("\r\n\r\n").unwrap_or((&r.data, ""));
    let unfolded = unfold(headers);
    let subject = unfolded.lines().find_map(|l| l.strip_prefix("Subject: ")).map(decode_words).unwrap_or_default();
    let boundary = unfolded
        .split("boundary=")
        .nth(1)
        .map(|b| {
            b.trim_matches(|c| c == '"' || c == ';' || c == '\n')
                .split(['"', ';', '\n'])
                .next()
                .unwrap_or("")
                .to_owned()
        })
        .unwrap_or_default();
    let (mut text, mut html) = (String::new(), String::new());
    for part in body.split(&format!("--{boundary}")) {
        let Some((ph, pb)) = part.trim_start_matches("\r\n").split_once("\r\n\r\n") else { continue };
        let ph = unfold(ph).to_lowercase();
        let decoded = if ph.contains("content-transfer-encoding: base64") {
            base64(pb)
        } else if ph.contains("content-transfer-encoding: quoted-printable") {
            quoted_printable(pb)
        } else {
            pb.as_bytes().to_vec()
        };
        let decoded = String::from_utf8_lossy(&decoded).replace("\r\n", "\n");
        if ph.contains("text/plain") {
            text = decoded;
        } else if ph.contains("text/html") {
            html = decoded;
        }
    }
    Message { headers: headers.to_owned(), subject, text, html }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cfg() -> WorkflowActionsConfig {
    WorkflowActionsConfig { poll: Duration::from_millis(100), ..WorkflowActionsConfig::default() }
}

fn mail_cfg(default_locale: &'static str) -> MailConfig {
    MailConfig {
        enabled: true,
        host: Some("127.0.0.1".into()),
        from: Some("\"ShadouCMDB\" <cmdb@corp.example>".into()),
        default_locale,
        allow_external_addresses: true,
        allowed_domains: vec!["corp.example".into()],
        timeout: Duration::from_secs(5),
        public_url: Some("https://cmdb.corp.example".into()),
        ..MailConfig::default()
    }
}

struct Mailer {
    sink: Sink,
    mail: Arc<Mail>,
    channels: Channels,
    channel: Channel,
}

async fn mailer(pool: &PgPool, mail: MailConfig) -> Mailer {
    let sink = Sink::start().await;
    let transport = Arc::new(Mail::plain_for_tests(&mail, "127.0.0.1", sink.addr.port()));
    let channel = super::email::channel(pool.clone(), transport.clone());
    let channels = Channels::default().with_mail(mail).with(WorkflowActionKind::Email, channel.clone());
    Mailer { sink, mail: transport, channels, channel }
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// Fans out every pending run, as one worker would.
async fn drain(pool: &PgPool, m: &Mailer) -> usize {
    let mut n = 0;
    loop {
        let ids = outbox::claim_runs(pool, "test-worker", 100).await.unwrap();
        if ids.is_empty() {
            return n;
        }
        for id in ids {
            assert_ne!(outbox::fan_out(pool, &cfg(), &m.channels, id, "test-worker").await.unwrap(), FanOut::Lost);
            n += 1;
        }
    }
}

/// Sends every e-mail delivery that is due (with `now`, also those waiting
/// for a bulk grace or the end of the hour), as one worker would; returns
/// the attempts made.
async fn send(pool: &PgPool, m: &Mailer, now: bool) -> usize {
    if now {
        ok(pool, "UPDATE workflow_action_deliveries SET next_attempt_at = now() WHERE status = 'pending'").await;
    }
    let mut n = 0;
    loop {
        let claimed = outbox::claim_deliveries(pool, "test-sender", WorkflowActionKind::Email, m.channel.timeout, 50)
            .await
            .unwrap();
        if claimed.is_empty() {
            return n;
        }
        for c in claimed {
            let outcome = (m.channel.send)(c.clone()).await;
            assert!(outbox::record(pool, &cfg(), &c, outcome).await.unwrap());
            n += 1;
        }
    }
}

fn actions(w: &World) -> String {
    format!("{DEFS}/{}/actions", w.definition)
}

async fn put_actions(w: &World, list: Value) -> Value {
    let v = w.ok("GET", &actions(w), json!(null)).await;
    w.ok("PUT", &actions(w), json!({ "version": v["version"], "actions": list })).await
}

async fn set_locale(pool: &PgPool, user: Uuid, locale: Option<&str>) {
    sqlx::query("UPDATE users SET locale = $2 WHERE id = $1").bind(user).bind(locale).execute(pool).await.unwrap();
}

/// Starts the workflow on server CI `ci` (environment prod) as the administrator and runs `approve` as `approver`.
async fn approve(w: &World, approver: &crate::modules::api_tokens::tests::Creds, ci: Uuid) {
    let (status, v) = w.start(&w.admin, ci).await;
    assert_eq!(status, 201, "{v}");
    let instance = v["instance"]["id"].as_str().unwrap();
    let (status, v) = w
        .call(
            approver,
            "POST",
            &format!("/api/v1/workflow-instances/{instance}/transitions"),
            Some(json!({ "transitionKey": "approve", "expectedVersion": 1,
                "fields": { "owner_team": "ops" }, "comment": "CAB approved" })),
        )
        .await;
    assert_eq!(status, 200, "{v}");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Who receives what: a recipient who may not view the CI's class gets
/// nothing; the others get the message in their language (NULL: the
/// default), and a reference to a CI they may not view reads "a CI you
/// cannot view"; a fixed address gets minimal content; a CR/LF in a CI label
/// adds no header; the CI owner, a Person field and the starter are reached.
#[tokio::test]
async fn email_reaches_viewers_in_their_language_and_names_nothing_they_cannot_view() {
    let Some(db) = scratch::database("workflow_email_recipients").await else { return };
    let mut w = world(&db).await;
    let m = mailer(&w.pool, mail_cfg("en")).await;
    // The actions API reads the address gate from the server's mail settings.
    w.app = crate::modules::api_tokens::tests::app_with_mail(w.pool.clone(), m.mail.clone());
    let person = id(&w.pool, "SELECT id FROM ci_classes WHERE system_role = 'person'").await;
    for (key, target) in [("uplink", w.network), ("contact", person), ("manager", person)] {
        w.ok(
            "POST",
            "/api/v1/attribute-definitions",
            json!({ "classId": w.server, "key": key, "label": key, "dataType": "reference", "referenceClassId": target }),
        )
        .await;
    }
    let manager = id(&w.pool, "SELECT id FROM ci_attribute_definitions WHERE key = 'manager'").await;
    ok(&w.pool, &format!("UPDATE ci_classes SET owner_attribute_id = '{manager}' WHERE id = '{}'", w.server)).await;
    // First in the type's order, so the uplink is in the message's field summary.
    ok(&w.pool, "UPDATE ci_attribute_definitions SET sort_order = -1 WHERE key = 'uplink'").await;

    let servers = w.profile("Servers", &[(w.server, false)]).await;
    let everything = w.profile("Everything", &[(w.server, false), (w.network, false), (person, false)]).await;
    let networks = w.profile("Networks only", &[(w.network, false)]).await;
    let (approver, approver_id) = w.user("approver", &[w.approvers]).await;
    let (_, en) = w.user("en_user", &[servers]).await;
    let (_, de) = w.user("de_user", &[servers]).await;
    w.user("all_user", &[everything]).await;
    let (_, blind) = w.user("blind_user", &[networks]).await;
    let (_, owner) = w.user("owner_user", &[servers]).await;
    let (_, contact) = w.user("contact_user", &[servers]).await;
    set_locale(&w.pool, de, Some("de")).await;
    set_locale(&w.pool, en, None).await;
    let person_of = |u: Uuid| {
        let pool = w.pool.clone();
        async move { id(&pool, &format!("SELECT person_ci_id FROM users WHERE id = '{u}'")).await }
    };

    let v = put_actions(
        &w,
        json!([{ "key": "tell", "name": "Tell the team", "kind": "email", "trigger": "transition",
            "transition": "approve",
            "recipients": [
                { "source": "profile", "profile": "Servers" }, { "source": "user", "user": "all_user" },
                { "source": "user", "user": "blind_user" }, { "source": "ci_owner" },
                { "source": "ci_attribute", "attribute": "contact" },
                { "source": "participant", "participant": "starter" },
                { "source": "address", "address": "CAB@corp.example" }],
            "settings": { "content": "detailed",
                "subject": { "en": "Approved: {{ci.label}}", "de": "Genehmigt: {{ci.label}}" },
                "intro": { "en": "CAB note for {{ci.label}}.", "de": "CAB-Hinweis zu {{ci.label}}." } } }]),
    )
    .await;
    let codes: Vec<&str> = v["problems"].as_array().unwrap().iter().map(|p| p["code"].as_str().unwrap()).collect();
    assert_eq!(codes, ["recipients_cannot_view"], "blind_user may not view servers: {v}");
    assert_eq!(v["actions"][0]["recipients"][6]["address"], "cab@corp.example");

    // The CI: an uplink to a network CI, the owner and contact Persons, and a label with a line break.
    let network = w.ci(w.network).await;
    ok(&w.pool, &format!("UPDATE configuration_items SET label = 'core-switch-7' WHERE id = '{network}'")).await;
    let ci = w.ci(w.server).await;
    let body = json!({ "attributes": { "environment": "prod", "uplink": network, "manager": person_of(owner).await,
        "contact": person_of(contact).await } });
    w.ok("PATCH", &format!("/api/v1/configuration-items/{ci}"), body).await;
    approve(&w, &approver, ci).await;
    assert_eq!(drain(&w.pool, &m).await, 1);
    // Read when the message is written: a label with a line break, as a title field could carry.
    ok(
        &w.pool,
        &format!("UPDATE configuration_items SET label = E'db01\\r\\nBcc: injected@evil.example' WHERE id = '{ci}'"),
    )
    .await;
    assert_eq!(m.sink.received().len(), 0, "nothing is sent by the fan-out");

    let deliveries: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT recipient_key, status, status_reason FROM workflow_action_deliveries ORDER BY recipient_key",
    )
    .fetch_all(&w.pool)
    .await
    .unwrap();
    let reason = |key: String| deliveries.iter().find(|d| d.0 == key).map(|d| (d.1.clone(), d.2.clone()));
    assert_eq!(reason(format!("user:{blind}")), Some(("skipped".into(), Some("no_view".into()))));
    assert_eq!(reason(format!("user:{approver_id}")), None, "the approver is not a recipient");
    assert_eq!(reason("addr:cab@corp.example".into()), Some(("pending".into(), None)));

    assert_eq!(send(&w.pool, &m, false).await, 7, "en, de, all, owner, contact, the starter (admin) and the address");
    assert!(m.sink.to("blind_user@example.test").is_empty(), "no view: nothing at all");
    let received = m.sink.received();
    assert!(received.iter().all(|r| r.to.len() == 1), "one recipient per message");
    assert!(!received.iter().any(|r| r.to.iter().any(|t| t.contains("evil"))), "no injected recipient");
    for r in &received {
        let msg = parse(r);
        assert!(!unfold(&msg.headers).lines().any(|l| l.starts_with("Bcc")), "no injected header: {}", msg.headers);
        assert_eq!(msg.header("Auto-Submitted").as_deref(), Some("auto-generated"));
        assert_eq!(msg.header("X-Auto-Response-Suppress").as_deref(), Some("All"));
        let id = msg.header("Message-ID").unwrap();
        assert!(id.ends_with("@cmdb.corp.example>"), "{id}");
    }

    // English (NULL locale, default en) and German.
    let en_msg = &m.sink.to("en_user@example.test")[0];
    assert!(en_msg.subject.starts_with("[") && en_msg.subject.contains("] Approved: db01 Bcc: injected@evil.example"));
    assert!(en_msg.text.contains("CAB note for db01"), "{}", en_msg.text);
    assert!(en_msg.text.contains("You receive this message because you hold the permission profile Servers"));
    assert!(en_msg.text.contains("https://cmdb.corp.example/workflows/"));
    let de_msg = &m.sink.to("de_user@example.test")[0];
    assert!(de_msg.subject.contains("] Genehmigt: db01"), "{}", de_msg.subject);
    assert!(de_msg.text.contains("Sie erhalten diese Nachricht, weil Sie das Berechtigungsprofil Servers haben"));
    assert!(de_msg.text.contains("Übergang"), "{}", de_msg.text);

    // Redaction: the uplink names a network CI; only who may view networks sees it.
    for to in ["en_user@example.test", "de_user@example.test", "owner_user@example.test"] {
        let msg = &m.sink.to(to)[0];
        assert!(!msg.all().contains("core-switch-7"), "{to} may not view networks: {}", msg.text);
    }
    assert!(en_msg.text.contains("a CI you cannot view"), "{}", en_msg.text);
    assert!(de_msg.text.contains("ein CI, das Sie nicht sehen dürfen"), "{}", de_msg.text);
    let all_msg = &m.sink.to("all_user@example.test")[0];
    assert!(
        all_msg.text.contains("core-switch-7") && !all_msg.text.contains("a CI you cannot view"),
        "{}",
        all_msg.text
    );

    // The CI owner (the type's owner field), the Person field and the starter.
    assert!(m.sink.to("owner_user@example.test")[0].text.contains("you are the owner of the configuration item"));
    assert!(m.sink.to("contact_user@example.test")[0].text.contains("you are named in the field contact"));
    assert!(m.sink.to("admin@example.test")[0].text.contains("you started the workflow"));

    // A fixed address: minimal content only.
    let cab = &m.sink.to("cab@corp.example")[0];
    let ident: String = sqlx::query_scalar("SELECT ident FROM configuration_items WHERE id = $1")
        .bind(ci)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    for leak in ["db01", ident.as_str(), "core-switch", "CAB approved", "ops", "approver"] {
        assert!(!cab.all().contains(leak), "minimal content names {leak}: {}", cab.all());
    }
    assert_eq!(cab.subject, "Approved:", "the CI placeholders are empty");
    assert!(cab.text.contains("Server lifecycle") && cab.text.contains("Approve"), "{}", cab.text);
    assert!(cab.text.contains("this address is configured to receive notices from workflow Server lifecycle"));

    let delivered = count(&w.pool, "SELECT count(*) FROM workflow_action_deliveries WHERE status = 'delivered'").await;
    assert_eq!(delivered, 7);
    db.drop().await;
}

/// The relay's 451 is retried with backoff and then delivered; its 550 is
/// dead for that recipient only, with an audit row; the others are sent. The
/// relay's error names the address, and neither the stored error nor the
/// detail shows it in clear (N-Q3). A dead delivery retried after its user
/// was deactivated sends nothing: the sender checks again on every attempt.
#[tokio::test]
async fn a_451_is_retried_and_a_550_is_dead_for_that_recipient_only() {
    let Some(db) = scratch::database("workflow_email_smtp_errors").await else { return };
    let mut w = world(&db).await;
    let m = mailer(&w.pool, mail_cfg("en")).await;
    w.app = crate::modules::api_tokens::tests::app_with_mail(w.pool.clone(), m.mail.clone());
    let servers = w.profile("Servers", &[(w.server, false)]).await;
    let (approver, _) = w.user("approver", &[w.approvers]).await;
    for name in ["busy", "gone", "fine"] {
        w.user(name, &[servers]).await;
    }
    m.sink.reply("busy@example.test", &[451]);
    m.sink.reply("gone@example.test", &[550]);
    m.sink.reply("carol@corp.example", &[550]);
    put_actions(
        &w,
        json!([{ "key": "tell", "name": "Tell", "kind": "email", "trigger": "transition", "transition": "approve",
            "recipients": [{ "source": "profile", "profile": "Servers" },
                { "source": "address", "address": "carol@corp.example" }] }]),
    )
    .await;
    let ci = w.ci(w.server).await;
    w.ok("PATCH", &format!("/api/v1/configuration-items/{ci}"), json!({ "attributes": { "environment": "prod" } }))
        .await;
    approve(&w, &approver, ci).await;
    drain(&w.pool, &m).await;
    assert_eq!(send(&w.pool, &m, false).await, 4);

    type Row = (String, String, Option<String>, i16, Option<i32>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT u.username, d.status, d.status_reason, d.attempts, d.last_status_code
         FROM workflow_action_deliveries d JOIN users u ON u.id = d.user_id ORDER BY u.username",
    )
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            ("busy".into(), "pending".into(), None, 1, Some(451)),
            ("fine".into(), "delivered".into(), None, 1, Some(250)),
            ("gone".into(), "dead".into(), Some("smtp_rejected".into()), 1, Some(550)),
        ]
    );
    let wait: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM next_attempt_at - now())::float8 FROM workflow_action_deliveries
         WHERE status = 'pending'",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert!((20.0..=40.0).contains(&wait), "first retry after about 30 s, got {wait}");
    let dead = count(&w.pool, "SELECT count(*) FROM audit_log WHERE action = 'workflow.action_dead'").await;
    assert_eq!(dead, 2, "gone and carol");
    let audit: Vec<Value> = sqlx::query_scalar("SELECT new_value FROM audit_log WHERE action = 'workflow.action_dead'")
        .fetch_all(&w.pool)
        .await
        .unwrap();
    for a in &audit {
        assert!(!a.to_string().contains("gone@") && !a.to_string().contains("carol@"), "no address in {a}");
    }

    // The relay's reply names the address; it is stored and shown masked only.
    let (carol, stored): (Uuid, String) = sqlx::query_as(
        "SELECT id, last_error FROM workflow_action_deliveries WHERE recipient_key = 'addr:carol@corp.example'",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert!(stored.contains("c***@corp.example") && !stored.contains("carol@"), "{stored}");
    let deliveries = format!("{DEFS}/{}/action-deliveries", w.definition);
    let v = w.ok("GET", &format!("{deliveries}/{carol}"), json!(null)).await;
    assert_eq!(v["delivery"]["recipient"]["address"], "c***@corp.example");
    assert!(!v.to_string().contains("carol@"), "the clear address in the detail: {v}");
    // A row stored before masking on write is masked when read.
    ok(
        &w.pool,
        &format!("UPDATE workflow_action_deliveries SET last_error = '550 <carol@corp.example>' WHERE id = '{carol}'"),
    )
    .await;
    let v = w.ok("GET", &format!("{deliveries}/{carol}"), json!(null)).await;
    assert_eq!(v["lastError"], "550 <c***@corp.example>");

    // The retry, when it is due.
    assert_eq!(send(&w.pool, &m, true).await, 1);
    assert_eq!(m.sink.to("busy@example.test").len(), 1);
    assert_eq!(m.sink.to("fine@example.test").len(), 1);
    assert_eq!(m.sink.to("gone@example.test").len(), 0);
    let busy: (String, i16) = sqlx::query_as(
        "SELECT d.status, d.attempts FROM workflow_action_deliveries d JOIN users u ON u.id = d.user_id
         WHERE u.username = 'busy'",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(busy, ("delivered".into(), 2));

    // Retried after the user was deactivated: checked again when sent, nothing goes out.
    let gone: Uuid = sqlx::query_scalar(
        "SELECT d.id FROM workflow_action_deliveries d JOIN users u ON u.id = d.user_id WHERE u.username = 'gone'",
    )
    .fetch_one(&w.pool)
    .await
    .unwrap();
    ok(&w.pool, "UPDATE users SET is_active = false WHERE username = 'gone'").await;
    m.sink.reply("gone@example.test", &[]);
    let (status, v) = w.call(&w.admin, "POST", &format!("{deliveries}/{gone}/retry"), None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(send(&w.pool, &m, true).await, 1);
    assert_eq!(m.sink.to("gone@example.test").len(), 0, "an inactive user gets nothing");
    let after: (String, Option<String>) =
        sqlx::query_as("SELECT status, status_reason FROM workflow_action_deliveries WHERE id = $1")
            .bind(gone)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(after, ("skipped".into(), Some("inactive".into())));
    db.drop().await;
}

/// The SQL fixture: an e-mail action on `n` instances (one CI each) of one
/// definition, for one user with an address who may view the class.
async fn mass(pool: &PgPool, n: i64, locale: Option<&str>) -> (Uuid, Vec<Uuid>) {
    let class = id(pool, "SELECT id FROM ci_classes WHERE system_role = 'business_service'").await;
    let first = id(
        pool,
        &format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', 'svc0000') RETURNING id"),
    )
    .await;
    let f = workflow_fixture(pool, "lifecycle", class, first, None).await;
    if n > 1 {
        ok(
            pool,
            &format!(
                "INSERT INTO configuration_items (class_id, label)
                   SELECT '{class}', 'svc' || lpad(i::text, 4, '0') FROM generate_series(1, {}) i;
                 INSERT INTO workflow_instances (definition_id, version_id, ci_id, current_state_id, status,
                   started_by_name)
                   SELECT '{}', '{}', c.id, '{}', 'active', 'test' FROM configuration_items c
                   WHERE c.class_id = '{class}' AND c.id <> '{first}'",
                n - 1,
                f.definition,
                f.version,
                f.planned
            ),
        )
        .await;
    }
    let user = id(
        pool,
        &format!(
            "WITH p AS (INSERT INTO configuration_items (class_id, label)
                          SELECT id, 'Alice' FROM ci_classes WHERE system_role = 'person' RETURNING id)
             INSERT INTO users (username, display_name, email, password_hash, locale, person_ci_id)
             SELECT 'alice', 'Alice', 'alice@corp.example', '$argon2id$v=19$test', {}, p.id FROM p RETURNING id",
            locale.map_or("NULL".into(), |l| format!("'{l}'"))
        ),
    )
    .await;
    let profile = id(pool, "INSERT INTO permission_profiles (name) VALUES ('Viewers') RETURNING id").await;
    ok(
        pool,
        &format!(
            "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view, can_create, can_edit,
               can_delete) VALUES ('{profile}', '{class}', true, false, false, false);
             INSERT INTO user_permission_profiles (user_id, profile_id) VALUES ('{user}', '{profile}');
             UPDATE workflow_action_queue_state SET max_per_instance_per_hour = 10000"
        ),
    )
    .await;
    let action = id(
        pool,
        &format!(
            "INSERT INTO workflow_actions (definition_id, key, name, kind, trigger, transition_key)
             VALUES ('{}', 'tell', 'Tell', 'email', 'transition', 'finish') RETURNING id",
            f.definition
        ),
    )
    .await;
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_action_recipients (action_id, position, source, user_id)
             VALUES ('{action}', 1, 'user', '{user}')"
        ),
    )
    .await;
    let instances: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM workflow_instances ORDER BY ci_id").fetch_all(pool).await.unwrap();
    (user, instances)
}

/// Transition events on these instances, `request` as their request id, in one transaction.
async fn transitions(pool: &PgPool, instances: &[Uuid], request: Option<&str>) {
    sqlx::query(
        "INSERT INTO workflow_instance_events (instance_id, kind, transition_key, from_state_key, to_state_key,
           to_version_no, actor_type, actor_name, request_id)
         SELECT i, 'transition', 'finish', 'planned', 'done', 1, 'system', 'bulk', $2 FROM unnest($1::uuid[]) i",
    )
    .bind(instances)
    .bind(request)
    .execute(pool)
    .await
    .unwrap();
}

/// 30 messages in an hour go out one by one; the 31st and later are folded
/// into one digest, sent when the hour ends.
#[tokio::test]
async fn the_31st_mail_in_an_hour_folds_into_one_digest() {
    let Some(db) = scratch::database("workflow_email_digest").await else { return };
    let pool = &db.pool;
    let m = mailer(pool, mail_cfg("de")).await;
    let (_, instances) = mass(pool, 1, None).await;
    // 34 transitions, each its own request.
    for _ in 0..34 {
        transitions(pool, &instances, None).await;
    }
    assert_eq!(drain(pool, &m).await, 34);
    let by: Vec<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT status, status_reason, count(*) FROM workflow_action_deliveries GROUP BY 1, 2 ORDER BY 1, 2",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(
        by,
        [("pending".into(), None, 31), ("skipped".into(), Some("throttled_digest".into()), 4)],
        "30 messages, one digest, 4 folded into it"
    );
    let due: bool = sqlx::query_scalar(
        "SELECT next_attempt_at = date_trunc('hour', now()) + interval '1 hour' FROM workflow_action_deliveries
         WHERE recipient_key LIKE 'digest:%'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(due, "the digest waits for the end of the hour");
    assert_eq!(send(pool, &m, false).await, 30, "the digest is not due yet");
    assert_eq!(send(pool, &m, true).await, 1);
    let got = m.sink.to("alice@corp.example");
    assert_eq!(got.len(), 31);
    let digest = got.iter().find(|g| g.subject.contains("Workflow-Benachrichtigungen")).expect("a digest");
    assert_eq!(digest.subject, "ShadouCMDB: 4 Workflow-Benachrichtigungen der letzten Stunde", "NULL: the default, de");
    assert_eq!(digest.text.matches("svc0000").count(), 4, "{}", digest.text);
    assert!(digest.text.contains("mehr als 30 Workflow-Nachrichten"), "{}", digest.text);
    db.drop().await;
}

/// A bulk transition of 2,000 CIs (four requests of 500 with one request
/// id) sends one message per recipient, listing 50 CIs and counting the rest.
#[tokio::test]
async fn a_2000_ci_bulk_transition_sends_one_mail_per_recipient() {
    let Some(db) = scratch::database("workflow_email_bulk").await else { return };
    let pool = &db.pool;
    let m = mailer(pool, mail_cfg("en")).await;
    let (_, instances) = mass(pool, 2000, Some("en")).await;
    assert_eq!(instances.len(), 2000);
    for chunk in instances.chunks(500) {
        transitions(pool, chunk, Some("bulk-request-1")).await;
    }
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_runs WHERE status = 'pending'").await, 2000);
    drain(pool, &m).await;
    assert_eq!(count(pool, "SELECT count(*) FROM workflow_action_runs WHERE status = 'fanned_out'").await, 2000);
    let leads = count(pool, "SELECT count(*) FROM workflow_action_deliveries WHERE status = 'pending'").await;
    assert_eq!(leads, 1);
    assert_eq!(send(pool, &m, false).await, 0, "the lead waits for the rest of the request");
    assert_eq!(send(pool, &m, true).await, 1);
    let got = m.sink.to("alice@corp.example");
    assert_eq!(got.len(), 1, "one message for 2,000 CIs");
    assert_eq!(got[0].subject, "Lifecycle lifecycle: Finish was applied to 2000 configuration items");
    assert!(got[0].text.contains("- svc0049 (") && !got[0].text.contains("svc0050"), "{}", got[0].text);
    assert!(got[0].text.contains("and 1950 more"), "{}", got[0].text);
    // Each run is fanned out once, and the counter took one message.
    let window: i32 =
        sqlx::query_scalar("SELECT count FROM workflow_action_rate_windows WHERE scope LIKE 'mail:user:%'")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(window, 1);
    db.drop().await;
}

/// With MAIL=off an e-mail action is fanned out, its deliveries skipped
/// `mail_off`, and nothing is sent.
#[tokio::test]
async fn mail_off_skips_and_sends_nothing() {
    let Some(db) = scratch::database("workflow_email_off").await else { return };
    let pool = &db.pool;
    let m = mailer(pool, MailConfig { enabled: false, ..mail_cfg("en") }).await;
    let (_, instances) = mass(pool, 1, None).await;
    transitions(pool, &instances, None).await;
    drain(pool, &m).await;
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT status, status_reason FROM workflow_action_deliveries").fetch_all(pool).await.unwrap();
    assert_eq!(rows, [("skipped".into(), Some("mail_off".into()))]);
    assert_eq!(send(pool, &m, true).await, 0);
    assert!(m.sink.received().is_empty());
    db.drop().await;
}
