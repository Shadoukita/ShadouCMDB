//! Ships audit_log rows to a SIEM or log collector (`AUDIT_EXPORT`).
//!
//! A background task follows the table by `chain_seq` (commit order, see
//! migration 0010) and sends each committed row once, as a JSON line or an
//! RFC 5424 syslog message. Reading the table instead of hooking the insert
//! calls means every writer is covered and a rolled-back change is never
//! exported. Each event carries its `rowHash`, so the collector holds an
//! off-host copy of the chain to compare with `shadoucmdb audit-verify`.
//!
//! Delivery is at-least-once while the process runs: a failed send is retried
//! from the same row on the next poll. The position is not persisted; after a
//! restart export resumes at the newest row, and rows written while the server
//! was down stay in the database only (their gap shows in `chainSeq`).

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::config::{AuditExportConfig, AuditFormat, AuditSink};

const BATCH: i64 = 500;
const APP_NAME: &str = "shadoucmdb";
/// RFC 5424 SD-ID. 32473 is the enterprise number RFC 5612 reserves for
/// documentation and examples; ShadouCMDB has none of its own.
const SD_ID: &str = "audit@32473";

/// One audit_log row, as exported.
#[derive(Debug, Clone)]
pub struct Event {
    pub chain_seq: i64,
    pub id: i64,
    pub occurred_at: DateTime<Utc>,
    pub actor_type: String,
    pub actor_id: Option<String>,
    pub actor_name: Option<String>,
    pub action: String,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub old_value: Option<Value>,
    pub new_value: Option<Value>,
    pub request_id: Option<String>,
    pub prev_hash: String,
    pub row_hash: String,
}

impl Event {
    fn to_json(&self) -> Value {
        json!({
            "source": APP_NAME,
            "chainSeq": self.chain_seq,
            "id": self.id,
            "occurredAt": self.occurred_at.to_rfc3339_opts(SecondsFormat::Micros, true),
            "actorType": self.actor_type,
            "actorId": self.actor_id,
            "actorName": self.actor_name,
            "action": self.action,
            "entityType": self.entity_type,
            "entityId": self.entity_id,
            "oldValue": self.old_value,
            "newValue": self.new_value,
            "requestId": self.request_id,
            "prevHash": self.prev_hash,
            "rowHash": self.row_hash,
        })
    }

    /// Warning for refused and locked sign-ins, notice for everything else.
    fn severity(&self) -> u8 {
        match self.action.as_str() {
            "login.failure" | "login.locked" => 4,
            _ => 5,
        }
    }
}

/// SD-PARAM value escaping (RFC 5424 §6.3.3).
fn sd_escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        if matches!(c, '"' | '\\' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// RFC 5424 header fields allow printable US-ASCII only, `-` when empty.
fn header_field(v: &str, max: usize) -> String {
    let s: String = v.chars().filter(|c| c.is_ascii_graphic()).take(max).collect();
    if s.is_empty() { "-".into() } else { s }
}

pub fn format_rfc5424(e: &Event, facility: u8, hostname: &str) -> String {
    let pri = u16::from(facility) * 8 + u16::from(e.severity());
    let mut sd = format!(
        "[{SD_ID} seq=\"{}\" id=\"{}\" actorType=\"{}\" entityType=\"{}\" entityId=\"{}\" hash=\"{}\"",
        e.chain_seq,
        e.id,
        sd_escape(&e.actor_type),
        sd_escape(&e.entity_type),
        e.entity_id,
        e.row_hash
    );
    if let Some(actor) = &e.actor_id {
        sd.push_str(&format!(" actorId=\"{}\"", sd_escape(actor)));
    }
    sd.push(']');
    format!(
        "<{pri}>1 {} {} {APP_NAME} {} {} {sd} \u{feff}{}",
        e.occurred_at.to_rfc3339_opts(SecondsFormat::Micros, true),
        header_field(hostname, 255),
        std::process::id(),
        header_field(&e.action, 32),
        e.to_json()
    )
}

fn hostname() -> String {
    ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_owned()))
        .unwrap_or_default()
}

enum Conn {
    Stdout(tokio::io::Stdout),
    File(tokio::fs::File),
    Udp(tokio::net::UdpSocket),
    Tcp(tokio::net::TcpStream),
}

struct Sink {
    target: AuditSink,
    format: AuditFormat,
    facility: u8,
    hostname: String,
    conn: Option<Conn>,
}

impl Sink {
    async fn open(target: &AuditSink) -> std::io::Result<Conn> {
        Ok(match target {
            AuditSink::Stdout => Conn::Stdout(tokio::io::stdout()),
            AuditSink::File(path) => Conn::File(open_append(path).await?),
            AuditSink::Udp(addr) => {
                let peer = tokio::net::lookup_host(addr.as_str())
                    .await?
                    .next()
                    .ok_or_else(|| std::io::Error::other(format!("{addr} does not resolve")))?;
                let local = if peer.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
                let socket = tokio::net::UdpSocket::bind(local).await?;
                socket.connect(peer).await?;
                Conn::Udp(socket)
            }
            AuditSink::Tcp(addr) => {
                let stream =
                    tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(addr.as_str()))
                        .await
                        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))??;
                stream.set_nodelay(true)?;
                Conn::Tcp(stream)
            }
        })
    }

    fn render(&self, e: &Event) -> String {
        match self.format {
            AuditFormat::Json => e.to_json().to_string(),
            AuditFormat::Rfc5424 => format_rfc5424(e, self.facility, &self.hostname),
        }
    }

    async fn send(&mut self, e: &Event) -> std::io::Result<()> {
        if self.conn.is_none() {
            self.conn = Some(Self::open(&self.target).await?);
        }
        let msg = self.render(e);
        let result = match self.conn.as_mut().expect("opened above") {
            Conn::Stdout(out) => write_line(out, &msg).await,
            Conn::File(f) => write_line(f, &msg).await,
            // One message per datagram (RFC 5426).
            Conn::Udp(s) => s.send(msg.as_bytes()).await.map(|_| ()),
            Conn::Tcp(s) => {
                let framed = match self.format {
                    // Octet counting (RFC 6587 §3.4.1): messages may contain newlines.
                    AuditFormat::Rfc5424 => format!("{} {msg}", msg.len()),
                    AuditFormat::Json => format!("{msg}\n"),
                };
                tokio::time::timeout(Duration::from_secs(10), s.write_all(framed.as_bytes()))
                    .await
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "write timed out"))?
            }
        };
        if result.is_err() {
            // Reconnect (or reopen the file, e.g. after log rotation) on the next attempt.
            self.conn = None;
        }
        result
    }
}

async fn open_append(path: &PathBuf) -> std::io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::new().create(true).append(true).open(path).await
}

async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, msg: &str) -> std::io::Result<()> {
    w.write_all(msg.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await
}

async fn head(pool: &PgPool) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT coalesce(max(chain_seq), 0) FROM audit_log").fetch_one(pool).await
}

async fn fetch_after(pool: &PgPool, after: i64) -> sqlx::Result<Vec<Event>> {
    let rows = sqlx::query(
        "SELECT chain_seq, id, occurred_at, actor_type, actor_id, actor_name, action, entity_type, entity_id,
                old_value, new_value, request_id, encode(prev_hash, 'hex') AS prev_hash,
                encode(row_hash, 'hex') AS row_hash
         FROM audit_log WHERE chain_seq > $1 ORDER BY chain_seq LIMIT $2",
    )
    .bind(after)
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok(Event {
                chain_seq: r.try_get("chain_seq")?,
                id: r.try_get("id")?,
                occurred_at: r.try_get("occurred_at")?,
                actor_type: r.try_get("actor_type")?,
                actor_id: r.try_get("actor_id")?,
                actor_name: r.try_get("actor_name")?,
                action: r.try_get("action")?,
                entity_type: r.try_get("entity_type")?,
                entity_id: r.try_get("entity_id")?,
                old_value: r.try_get("old_value")?,
                new_value: r.try_get("new_value")?,
                request_id: r.try_get("request_id")?,
                prev_hash: r.try_get("prev_hash")?,
                row_hash: r.try_get("row_hash")?,
            })
        })
        .collect()
}

/// Logs a failure once, not on every poll, and the recovery when it ends.
#[derive(Default)]
struct Health {
    failing: Option<&'static str>,
}

impl Health {
    fn fail(&mut self, what: &'static str, error: &dyn std::fmt::Display) {
        if self.failing != Some(what) {
            tracing::error!(error = %error, "audit export: {what} failed; retrying");
            self.failing = Some(what);
        }
    }
    fn ok(&mut self) {
        if self.failing.take().is_some() {
            tracing::info!("audit export recovered");
        }
    }
}

/// Sends everything after `cursor`; returns the new cursor.
async fn drain(pool: &PgPool, sink: &mut Sink, mut cursor: i64, health: &mut Health) -> i64 {
    loop {
        let events = match fetch_after(pool, cursor).await {
            Ok(events) => events,
            Err(e) => {
                health.fail("reading audit_log", &e);
                return cursor;
            }
        };
        let full = events.len() as i64 == BATCH;
        for e in &events {
            if let Err(err) = sink.send(e).await {
                health.fail("sending", &err);
                return cursor;
            }
            cursor = e.chain_seq;
        }
        health.ok();
        if !full {
            return cursor;
        }
    }
}

async fn run(pool: PgPool, cfg: AuditExportConfig, mut stop: watch::Receiver<bool>) {
    let mut sink =
        Sink { target: cfg.sink, format: cfg.format, facility: cfg.facility, hostname: hostname(), conn: None };
    let mut health = Health::default();
    let mut cursor = None;
    loop {
        cursor = match cursor {
            None => match head(&pool).await {
                Ok(seq) => {
                    tracing::info!(after_chain_seq = seq, "audit export started");
                    Some(seq)
                }
                Err(e) => {
                    health.fail("reading audit_log", &e);
                    None
                }
            },
            Some(c) => Some(drain(&pool, &mut sink, c, &mut health).await),
        };
        tokio::select! {
            _ = tokio::time::sleep(cfg.poll_interval) => {}
            _ = stop.changed() => {
                // Last pass, so changes made by the final requests leave too.
                if let Some(c) = cursor {
                    drain(&pool, &mut sink, c, &mut health).await;
                }
                return;
            }
        }
    }
}

pub struct Exporter {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Exporter {
    /// Exports what is left and stops; gives up after 10 s.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        if tokio::time::timeout(Duration::from_secs(10), self.task).await.is_err() {
            tracing::warn!("audit export did not finish within 10 s");
        }
    }
}

pub fn spawn(pool: PgPool, cfg: AuditExportConfig) -> Exporter {
    tracing::info!(target = ?cfg.sink, format = ?cfg.format, "audit export enabled");
    let (stop, rx) = watch::channel(false);
    Exporter { stop, task: tokio::spawn(run(pool, cfg, rx)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(action: &str) -> Event {
        Event {
            chain_seq: 42,
            id: 7,
            occurred_at: DateTime::parse_from_rfc3339("2026-09-27T08:15:30.123456Z").unwrap().with_timezone(&Utc),
            actor_type: "user".into(),
            actor_id: Some("a\"b]c".into()),
            actor_name: Some("alice".into()),
            action: action.into(),
            entity_type: "configuration_items".into(),
            entity_id: Uuid::nil(),
            old_value: None,
            new_value: Some(json!({"name": "web-01\nline"})),
            request_id: Some("req-1".into()),
            prev_hash: "00".repeat(32),
            row_hash: "ab".repeat(32),
        }
    }

    #[test]
    fn rfc5424_message_shape() {
        let msg = format_rfc5424(&event("create"), 13, "cmdb host");
        // facility 13 (log audit) * 8 + severity 5 (notice)
        assert!(msg.starts_with("<109>1 2026-09-27T08:15:30.123456Z cmdbhost shadoucmdb "), "{msg}");
        assert!(msg.contains(" create [audit@32473 seq=\"42\" id=\"7\" actorType=\"user\""), "{msg}");
        assert!(msg.contains(r#"actorId="a\"b\]c"]"#), "SD values are escaped: {msg}");
        assert!(msg.contains(&format!("hash=\"{}\"", "ab".repeat(32))), "{msg}");
        let body = msg.split_once(" \u{feff}").expect("BOM before MSG").1;
        let v: Value = serde_json::from_str(body).unwrap();
        assert_eq!(v["chainSeq"], 42);
        assert_eq!(v["newValue"]["name"], "web-01\nline");
        assert!(!body.contains('\n'), "JSON escapes newlines, one message per line");
    }

    #[test]
    fn failed_sign_ins_are_warnings() {
        assert!(format_rfc5424(&event("login.failure"), 13, "h").starts_with("<108>1 "));
        assert!(format_rfc5424(&event("login.success"), 4, "h").starts_with("<37>1 "));
        assert!(format_rfc5424(&event("create"), 13, "").contains(" - shadoucmdb "), "empty host is -");
    }
}
