//! Ships audit_log rows to a SIEM or log collector (`AUDIT_EXPORT`).
//!
//! A background task follows the table by `chain_seq` (commit order, see
//! migration 0018) and sends each committed row once, as a JSON line or an
//! RFC 5424 syslog message. Reading the table instead of hooking the insert
//! calls means every writer is covered and a rolled-back change is never
//! exported. Each event carries its `rowHash`, so the collector holds an
//! off-host copy of the chain to compare with `shadoucmdb audit-verify`.
//!
//! Delivery is at-least-once while the process runs: a failed send is retried
//! from the same row on the next poll. The position is not persisted; after a
//! restart export resumes at the newest row, and rows written while the server
//! was down stay in the database only (their gap shows in `chainSeq`). The
//! exception is the `backup.restore` entry that `shadoucmdb restore` leaves,
//! so the collector learns which head the restore went back to (GH#513):
//! export starts before the oldest such entry not yet sent, so it leaves with
//! every row after it, also the ones CLI commands (`mfa reset-undecryptable`,
//! `create-admin`) wrote before the server started (GH#677). A sent entry is
//! listed in `audit_export_restores` (migration 0061; only existing entries
//! may be listed, 0063, GH#696); one whose send fails is sent again after a
//! restart.
//!
//! A row that can never be sent (larger than one UDP datagram) must not hold
//! the export up: it leaves as a stub without `oldValue` and `newValue`, with
//! `oversize` set, keeping `chainSeq` and `rowHash` so the collector still has
//! the chain (GH#179).
//!
//! A refused sign-in (`login.failure`, `login.locked`) keeps the username as
//! typed in the table, and that may be a password typed into the wrong field
//! (GH#415). The export is read where the server log is (stdout is the
//! journal), so when the name matches no account it leaves as `null`, with
//! `attemptedUsernameRedacted` set (GH#509). The table keeps it for those
//! with `audit.view`; `rowHash` is of the stored row, as for an oversize stub.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use uuid::Uuid;

use crate::config::{AuditExportConfig, AuditFormat, AuditSink, sink_host};

const BATCH: i64 = 500;
const APP_NAME: &str = "shadoucmdb";
/// RFC 5424 SD-ID. 32473 is the enterprise number RFC 5612 reserves for
/// documentation and examples; ShadouCMDB has none of its own.
const SD_ID: &str = "audit@32473";
/// Largest UDP payload over IPv4 (65,535 - 8 byte UDP header - 20 byte IP header).
const UDP_MAX: usize = 65_507;

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

/// Actions whose `newValue.attemptedUsername` is the name as typed.
const ATTEMPTED_USERNAME_ACTIONS: [&str; 2] = ["login.failure", "login.locked"];

/// Blanks `attemptedUsername` in `new_value` when it named no account.
fn redact_attempted_username(new_value: &mut Option<Value>, known: bool) {
    let Some(Value::Object(fields)) = new_value else { return };
    if known || !fields.get("attemptedUsername").is_some_and(|v| !v.is_null()) {
        return;
    }
    fields.insert("attemptedUsername".into(), Value::Null);
    fields.insert("attemptedUsernameRedacted".into(), json!(true));
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

/// The row as sent when the whole of it cannot be: its values are left out.
fn oversize_stub(e: &Event, bytes: usize) -> Value {
    let mut v = e.to_json();
    v["oldValue"] = Value::Null;
    v["newValue"] = Value::Null;
    v["oversize"] = json!(true);
    v["originalBytes"] = json!(bytes);
    v
}

fn rfc5424(e: &Event, body: &Value, facility: u8, hostname: &str) -> String {
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
        body
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
    Tls(Box<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>),
}

struct Sink {
    target: AuditSink,
    format: AuditFormat,
    facility: u8,
    hostname: String,
    /// Set for `tls://`.
    tls: Option<TlsConnector>,
    conn: Option<Conn>,
}

/// TLS client for `tls://`: the public roots, the operating system's store and
/// `AUDIT_EXPORT_TLS_CA_FILE`, as for the identity-provider connections.
fn tls_connector(ca_file: Option<&PathBuf>) -> anyhow::Result<TlsConnector> {
    let pem = match ca_file {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("AUDIT_EXPORT_TLS_CA_FILE: cannot read {}: {e}", path.display()))?,
        ),
        None => None,
    };
    let config = crate::auth::sso::tls::client_config(pem.as_deref())
        .map_err(|e| anyhow::anyhow!("AUDIT_EXPORT_TLS_CA_FILE: {e}"))?;
    Ok(TlsConnector::from(Arc::new(config)))
}

async fn tcp_connect(addr: &str) -> std::io::Result<tokio::net::TcpStream> {
    let stream = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(addr))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out"))??;
    stream.set_nodelay(true)?;
    Ok(stream)
}

impl Sink {
    async fn open(&self) -> std::io::Result<Conn> {
        Ok(match &self.target {
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
            AuditSink::Tcp(addr) => Conn::Tcp(tcp_connect(addr).await?),
            AuditSink::Tls(addr) => {
                let connector = self.tls.as_ref().expect("built by spawn for tls://");
                let name = ServerName::try_from(sink_host(addr).to_owned())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
                let tcp = tcp_connect(addr).await?;
                let tls = tokio::time::timeout(Duration::from_secs(10), connector.connect(name, tcp))
                    .await
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
                Conn::Tls(Box::new(tls))
            }
        })
    }

    fn render(&self, e: &Event, body: &Value) -> String {
        match self.format {
            AuditFormat::Json => body.to_string(),
            AuditFormat::Rfc5424 => rfc5424(e, body, self.facility, &self.hostname),
        }
    }

    /// The message for `e`, or its stub when the sink cannot carry it whole.
    fn message(&self, e: &Event) -> String {
        let msg = self.render(e, &e.to_json());
        if !matches!(self.target, AuditSink::Udp(_)) || msg.len() <= UDP_MAX {
            return msg;
        }
        tracing::warn!(
            chain_seq = e.chain_seq,
            bytes = msg.len(),
            "audit export: row larger than a UDP datagram; sent without oldValue and newValue"
        );
        self.render(e, &oversize_stub(e, msg.len()))
    }

    async fn send(&mut self, e: &Event) -> std::io::Result<()> {
        if self.conn.is_none() {
            self.conn = Some(self.open().await?);
        }
        let msg = self.message(e);
        let result = match self.conn.as_mut().expect("opened above") {
            Conn::Stdout(out) => write_line(out, &msg).await,
            Conn::File(f) => write_line(f, &msg).await,
            // One message per datagram (RFC 5426).
            Conn::Udp(s) => s.send(msg.as_bytes()).await.map(|_| ()),
            Conn::Tcp(s) => write_framed(s, self.format, &msg).await,
            Conn::Tls(s) => write_framed(s.as_mut(), self.format, &msg).await,
        };
        if result.is_err() {
            // Reconnect (or reopen the file, e.g. after log rotation) on the next attempt.
            self.conn = None;
        }
        result
    }
}

/// One message on a stream transport, flushed (TLS buffers records).
async fn write_framed<W: AsyncWriteExt + Unpin>(w: &mut W, format: AuditFormat, msg: &str) -> std::io::Result<()> {
    let framed = match format {
        // Octet counting (RFC 6587 §3.4.1, RFC 5425 §4.3): messages may contain newlines.
        AuditFormat::Rfc5424 => format!("{} {msg}", msg.len()),
        AuditFormat::Json => format!("{msg}\n"),
    };
    let write = async {
        w.write_all(framed.as_bytes()).await?;
        w.flush().await
    };
    tokio::time::timeout(Duration::from_secs(10), write)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "write timed out"))?
}

/// Audit events are personal data: a new file is readable by owner and group
/// only, whatever the umask (GH#443). An existing file keeps its mode.
async fn open_append(path: &PathBuf) -> std::io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    options.mode(0o640);
    options.open(path).await
}

async fn write_line<W: AsyncWriteExt + Unpin>(w: &mut W, msg: &str) -> std::io::Result<()> {
    w.write_all(msg.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await
}

/// The first cursor: the newest row, or just before the oldest `backup.restore`
/// entry not yet sent; with the number of those entries.
async fn start(pool: &PgPool) -> sqlx::Result<(i64, i64)> {
    sqlx::query_as(
        "WITH unsent AS (
             SELECT a.chain_seq FROM audit_log a
             WHERE a.action = 'backup.restore'
               AND NOT EXISTS (SELECT 1 FROM audit_export_restores s WHERE s.chain_seq = a.chain_seq)
         )
         SELECT coalesce((SELECT min(chain_seq) - 1 FROM unsent), (SELECT max(chain_seq) FROM audit_log), 0),
                (SELECT count(*) FROM unsent)",
    )
    .fetch_one(pool)
    .await
}

/// Lists a sent `backup.restore` entry, so a restart does not go back to it.
async fn mark_restore_sent(pool: &PgPool, chain_seq: i64) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO audit_export_restores (chain_seq) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(chain_seq)
        .execute(pool)
        .await
        .map(|_| ())
}

#[cfg(test)]
async fn head(pool: &PgPool) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT coalesce(max(chain_seq), 0) FROM audit_log").fetch_one(pool).await
}

/// `attempted_known`: for a refused sign-in, whether the name it was given
/// matches an account now (case-insensitively, as sign-in matches it).
async fn fetch_after(pool: &PgPool, after: i64) -> sqlx::Result<Vec<Event>> {
    let rows = sqlx::query(
        "SELECT a.chain_seq, a.id, a.occurred_at, a.actor_type, a.actor_id, a.actor_name, a.action, a.entity_type,
                a.entity_id, a.old_value, a.new_value, a.request_id, encode(a.prev_hash, 'hex') AS prev_hash,
                encode(a.row_hash, 'hex') AS row_hash,
                a.action = ANY($3) AND EXISTS (
                    SELECT 1 FROM users u WHERE lower(u.username) = lower(a.new_value->>'attemptedUsername')
                ) AS attempted_known
         FROM audit_log a WHERE a.chain_seq > $1 ORDER BY a.chain_seq LIMIT $2",
    )
    .bind(after)
    .bind(BATCH)
    .bind(&ATTEMPTED_USERNAME_ACTIONS[..])
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|r| {
            let action: String = r.try_get("action")?;
            let mut new_value: Option<Value> = r.try_get("new_value")?;
            if ATTEMPTED_USERNAME_ACTIONS.contains(&action.as_str()) {
                redact_attempted_username(&mut new_value, r.try_get::<bool, _>("attempted_known")?);
            }
            Ok(Event {
                chain_seq: r.try_get("chain_seq")?,
                id: r.try_get("id")?,
                occurred_at: r.try_get("occurred_at")?,
                actor_type: r.try_get("actor_type")?,
                actor_id: r.try_get("actor_id")?,
                actor_name: r.try_get("actor_name")?,
                action,
                entity_type: r.try_get("entity_type")?,
                entity_id: r.try_get("entity_id")?,
                old_value: r.try_get("old_value")?,
                new_value,
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
            if e.action == "backup.restore"
                && let Err(err) = mark_restore_sent(pool, e.chain_seq).await
            {
                // It is sent; a restart would send it (and what follows) again.
                tracing::warn!(chain_seq = e.chain_seq, error = %err, "audit export: cannot record a sent backup.restore entry");
            }
        }
        health.ok();
        if !full {
            return cursor;
        }
    }
}

async fn run(pool: PgPool, cfg: AuditExportConfig, tls: Option<TlsConnector>, mut stop: watch::Receiver<bool>) {
    let mut sink =
        Sink { target: cfg.sink, format: cfg.format, facility: cfg.facility, hostname: hostname(), tls, conn: None };
    let mut health = Health::default();
    let mut cursor = None;
    loop {
        cursor = match cursor {
            None => match start(&pool).await {
                Ok((seq, restores)) => {
                    if restores > 0 {
                        tracing::warn!(
                            after_chain_seq = seq,
                            restores,
                            "audit export started before a backup.restore entry not yet sent; it leaves with every \
                             row after it"
                        );
                    } else {
                        tracing::info!(after_chain_seq = seq, "audit export started");
                    }
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

/// Whether `host` is this machine, so cleartext to it never crosses a network.
fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Starts the export; fails when the TLS trust anchors cannot be loaded.
pub fn spawn(pool: PgPool, cfg: AuditExportConfig) -> anyhow::Result<Exporter> {
    let tls = match cfg.sink {
        AuditSink::Tls(_) => Some(tls_connector(cfg.tls_ca_file.as_ref())?),
        _ => None,
    };
    tracing::info!(target = ?cfg.sink, format = ?cfg.format, "audit export enabled");
    if matches!(cfg.sink, AuditSink::Udp(_)) {
        tracing::warn!(
            "AUDIT_EXPORT uses UDP: delivery is not acknowledged, so datagrams the network or the collector drops \
             are lost without notice, and a row larger than {UDP_MAX} bytes is sent without its values. \
             Use tls:// (or tcp://) where the collector supports it."
        );
    }
    if let AuditSink::Udp(addr) | AuditSink::Tcp(addr) = &cfg.sink
        && !is_loopback(sink_host(addr))
    {
        tracing::warn!(
            "AUDIT_EXPORT sends audit events unencrypted to {addr}: anyone on the network path can read, alter or \
             drop them. Use tls://host:port, or a relay on this host that forwards over TLS."
        );
    }
    let (stop, rx) = watch::channel(false);
    Ok(Exporter { stop, task: tokio::spawn(run(pool, cfg, tls, rx)) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format_rfc5424(e: &Event, facility: u8, hostname: &str) -> String {
        rfc5424(e, &e.to_json(), facility, hostname)
    }

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

    async fn udp_sink(format: AuditFormat) -> (Sink, tokio::net::UdpSocket) {
        let collector = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = AuditSink::Udp(collector.local_addr().unwrap().to_string());
        (Sink { target, format, facility: 13, hostname: "h".into(), tls: None, conn: None }, collector)
    }

    async fn receive(collector: &tokio::net::UdpSocket) -> Value {
        let mut buf = vec![0u8; 70_000];
        let n = tokio::time::timeout(Duration::from_secs(5), collector.recv(&mut buf)).await.unwrap().unwrap();
        let msg = std::str::from_utf8(&buf[..n]).unwrap();
        let body = msg.split_once(" \u{feff}").map_or(msg, |(_, body)| body);
        serde_json::from_str(body).unwrap()
    }

    /// GH#179: a row larger than a datagram goes as a stub, and the next row follows.
    #[tokio::test]
    async fn oversize_rows_leave_as_a_stub_over_udp() {
        for format in [AuditFormat::Rfc5424, AuditFormat::Json] {
            let (mut sink, collector) = udp_sink(format).await;
            let mut big = event("token.use");
            big.new_value = Some(json!({ "path": "/".repeat(70_000) }));
            sink.send(&big).await.expect("an oversize row is not an error");
            let mut next = event("create");
            next.chain_seq = 43;
            sink.send(&next).await.unwrap();

            let stub = receive(&collector).await;
            assert_eq!((stub["chainSeq"].as_i64(), stub["oversize"].as_bool()), (Some(42), Some(true)), "{stub}");
            assert_eq!(stub["rowHash"].as_str(), Some("ab".repeat(32).as_str()));
            assert_eq!((stub["newValue"].clone(), stub["action"].as_str()), (Value::Null, Some("token.use")));
            assert!(stub["originalBytes"].as_u64().unwrap() > 70_000, "{stub}");
            let v = receive(&collector).await;
            assert_eq!((v["chainSeq"].as_i64(), v.get("oversize")), (Some(43), None), "{v}");
            assert_eq!(v["newValue"]["name"], "web-01\nline");
        }
    }

    /// GH#443: a new export file is not readable by other users, whatever the umask.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_audit_file_is_created_owner_and_group_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("shadoucmdb-audit-{}.jsonl", Uuid::new_v4()));
        let mut sink = Sink {
            target: AuditSink::File(path.clone()),
            format: AuditFormat::Json,
            facility: 13,
            hostname: "h".into(),
            tls: None,
            conn: None,
        };
        sink.send(&event("create")).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        std::fs::remove_file(&path).unwrap();
        assert_eq!(mode & !0o640, 0, "mode {mode:o} is 0640 or stricter");
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

    /// A one-connection TLS collector on localhost; yields what it received.
    async fn tls_collector(
        config: Arc<tokio_rustls::rustls::ServerConfig>,
    ) -> (u16, JoinHandle<std::io::Result<String>>) {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await?;
            let mut tls = tokio_rustls::TlsAcceptor::from(config).accept(tcp).await?;
            // The sink drops its connection without close_notify: keep what arrived before that.
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match tls.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(e) if buf.is_empty() => return Err(e),
                    Err(_) => break,
                }
            }
            Ok(String::from_utf8(buf).unwrap())
        });
        (port, task)
    }

    async fn tls_sink(port: u16, ca_pem: &str) -> Sink {
        let ca_file = std::env::temp_dir().join(format!("shadoucmdb-audit-ca-{}.pem", Uuid::new_v4()));
        std::fs::write(&ca_file, ca_pem).unwrap();
        let tls = tls_connector(Some(&ca_file)).unwrap();
        std::fs::remove_file(&ca_file).unwrap();
        Sink {
            target: AuditSink::Tls(format!("localhost:{port}")),
            format: AuditFormat::Rfc5424,
            facility: 13,
            hostname: "h".into(),
            tls: Some(tls),
            conn: None,
        }
    }

    /// GH#443: `tls://` delivers octet-counted RFC 5424 messages over a verified connection.
    #[tokio::test]
    async fn tls_sink_delivers_to_a_trusted_collector() {
        let (ca_pem, config) = tls_server("localhost");
        let (port, collector) = tls_collector(config).await;
        let mut sink = tls_sink(port, &ca_pem).await;
        sink.send(&event("create")).await.unwrap();
        sink.conn = None; // closes the connection, so the collector sees the end
        let received = collector.await.unwrap().unwrap();
        let (len, msg) = received.split_once(' ').unwrap();
        assert_eq!(len.parse::<usize>().unwrap(), msg.len(), "octet counting: {received}");
        assert!(msg.starts_with("<109>1 2026-09-27T08:15:30.123456Z h shadoucmdb "), "{msg}");
    }

    /// GH#443: a certificate for another host name is refused, and nothing is sent.
    #[tokio::test]
    async fn tls_sink_refuses_a_certificate_for_another_host() {
        let (ca_pem, config) = tls_server("siem.example.com");
        let (port, collector) = tls_collector(config).await;
        let mut sink = tls_sink(port, &ca_pem).await;
        let err = sink.send(&event("create")).await.unwrap_err();
        assert!(err.to_string().contains("certificate"), "{err}");
        assert!(sink.conn.is_none());
        assert!(collector.await.unwrap().is_err(), "the handshake did not complete");
    }

    /// GH#443: without the private CA the collector's certificate is not trusted.
    #[tokio::test]
    async fn tls_sink_refuses_an_untrusted_certificate() {
        let (_, config) = tls_server("localhost");
        let (other_ca, _) = tls_server("localhost");
        let (port, collector) = tls_collector(config).await;
        let err = tls_sink(port, &other_ca).await.send(&event("create")).await.unwrap_err();
        assert!(err.to_string().contains("certificate"), "{err}");
        assert!(collector.await.unwrap().is_err());
    }

    #[test]
    fn cleartext_warning_spares_loopback() {
        for host in ["localhost", "127.0.0.1", "::1"] {
            assert!(is_loopback(host), "{host}");
        }
        for host in ["siem.example.com", "10.0.0.5", "2001:db8::1"] {
            assert!(!is_loopback(host), "{host}");
        }
        assert!(tls_connector(Some(&PathBuf::from("/nonexistent/ca.pem"))).is_err());
    }

    /// GH#179: `drain` moves past an oversize row instead of retrying it forever.
    #[tokio::test]
    async fn drain_does_not_stop_at_an_oversize_row() {
        let Some(db) = crate::db::scratch::database("drain_does_not_stop_at_an_oversize_row").await else { return };
        let (mut sink, collector) = udp_sink(AuditFormat::Rfc5424).await;
        let start = head(&db.pool).await.unwrap();
        let ctx = crate::api::context::RequestContext::system("test", "req-179");
        let entry = |value: Value| crate::data::crud::AuditEntry {
            action: crate::data::crud::AuditAction::Create,
            entity_type: "configuration_items",
            entity_id: Uuid::new_v4(),
            old_value: None,
            new_value: Some(value),
        };
        let mut conn = db.pool.acquire().await.unwrap();
        let rows = vec![entry(json!({ "notes": "x".repeat(66_000) })), entry(json!({ "name": "after" }))];
        crate::data::crud::write_audit(&mut conn, &ctx, rows).await.unwrap();
        drop(conn);

        let mut health = Health::default();
        let cursor = drain(&db.pool, &mut sink, start, &mut health).await;
        assert_eq!(cursor, head(&db.pool).await.unwrap(), "the cursor passed both rows");
        assert!(health.failing.is_none());
        assert_eq!(receive(&collector).await["oversize"], json!(true));
        assert_eq!(receive(&collector).await["newValue"]["name"], "after");
        db.drop().await;
    }

    /// GH#677: a `backup.restore` entry followed by a CLI row before the server
    /// starts (`mfa reset-undecryptable` after a restore) is still sent, with
    /// that row; once sent, a restart resumes at the newest row.
    #[tokio::test]
    async fn an_unsent_restore_entry_is_sent_with_the_rows_after_it() {
        let Some(db) = crate::db::scratch::database("an_unsent_restore_entry_is_sent_with_the_rows_after_it").await
        else {
            return;
        };
        let insert = |action: &'static str| {
            sqlx::query_scalar::<_, i64>(
                "INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
                 VALUES ('system', session_user, $1, $2, gen_random_uuid(), '{}') RETURNING chain_seq",
            )
            .bind(action)
            .bind(if action == "backup.restore" { "audit_log" } else { "users" })
            .fetch_one(&db.pool)
        };
        // Exported before: a restart does not go back to it.
        let old = insert("backup.restore").await.unwrap();
        mark_restore_sent(&db.pool, old).await.unwrap();
        insert("mfa.disable").await.unwrap();
        assert_eq!(start(&db.pool).await.unwrap(), (head(&db.pool).await.unwrap(), 0));

        let restore = insert("backup.restore").await.unwrap();
        let after = insert("mfa.disable").await.unwrap();
        assert_eq!(start(&db.pool).await.unwrap(), (restore - 1, 1), "starts before the unsent entry");

        let path = std::env::temp_dir().join(format!("shadoucmdb-audit-{}.jsonl", Uuid::new_v4()));
        let mut sink = Sink {
            target: AuditSink::File(path.clone()),
            format: AuditFormat::Json,
            facility: 13,
            hostname: "h".into(),
            tls: None,
            conn: None,
        };
        let (cursor, _) = start(&db.pool).await.unwrap();
        let mut health = Health::default();
        assert_eq!(drain(&db.pool, &mut sink, cursor, &mut health).await, after);
        let out = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let sent: Vec<(i64, String)> = out
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .map(|v| (v["chainSeq"].as_i64().unwrap(), v["action"].as_str().unwrap().to_owned()))
            .collect();
        assert_eq!(sent, [(restore, "backup.restore".into()), (after, "mfa.disable".into())]);
        assert!(health.failing.is_none());
        assert_eq!(start(&db.pool).await.unwrap(), (after, 0), "a restart resumes at the newest row");
        db.drop().await;
    }

    fn file_sink() -> (Sink, PathBuf) {
        let path = std::env::temp_dir().join(format!("shadoucmdb-audit-{}.jsonl", Uuid::new_v4()));
        let sink = Sink {
            target: AuditSink::File(path.clone()),
            format: AuditFormat::Json,
            facility: 13,
            hostname: "h".into(),
            tls: None,
            conn: None,
        };
        (sink, path)
    }

    fn sql_state(err: &sqlx::Error) -> String {
        err.as_database_error().and_then(|d| d.code()).unwrap_or_default().into_owned()
    }

    /// GH#696: the API role lists only a `backup.restore` entry that exists,
    /// never a chainSeq past the head or another row, and `sent_at` is when it
    /// listed it.
    #[tokio::test]
    async fn the_api_role_lists_only_existing_restore_entries() {
        let Some(roles) = crate::db::scratch::Roles::create("the_api_role_lists_only_existing_restore_entries").await
        else {
            return;
        };
        let db = roles.database().await;
        let api = roles.api_pool(&db).await;
        let insert = |action: &'static str| {
            sqlx::query_scalar::<_, i64>(
                "INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
                 VALUES ('system', session_user, $1, 'audit_log', gen_random_uuid(), '{}') RETURNING chain_seq",
            )
            .bind(action)
            .fetch_one(&db.pool)
        };
        let other = insert("mfa.disable").await.unwrap();
        let head = head(&db.pool).await.unwrap();
        for seq in [head + 1, head + 250, other] {
            let err = mark_restore_sent(&api, seq).await.unwrap_err();
            assert_eq!(sql_state(&err), "23503", "chainSeq {seq}: {err}");
        }
        let planted = "INSERT INTO cmdb.audit_export_restores (chain_seq) SELECT generate_series($1, $1 + 249)";
        let err = sqlx::query(planted).bind(head + 1).execute(&api).await.unwrap_err();
        assert_eq!(sql_state(&err), "23503", "{err}");

        let restore = insert("backup.restore").await.unwrap();
        assert_eq!(start(&api).await.unwrap(), (restore - 1, 1));
        sqlx::query("INSERT INTO cmdb.audit_export_restores (chain_seq, sent_at) VALUES ($1, '2000-01-01')")
            .bind(restore)
            .execute(&api)
            .await
            .unwrap();
        let backdated: bool = sqlx::query_scalar(
            "SELECT sent_at < now() - interval '1 minute' FROM cmdb.audit_export_restores WHERE chain_seq = $1",
        )
        .bind(restore)
        .fetch_one(&api)
        .await
        .unwrap();
        assert!(!backdated, "sent_at is the time of the insert");
        assert_eq!(start(&api).await.unwrap(), (restore, 0));
        api.close().await;
        db.drop().await;
        roles.drop().await;
    }

    /// GH#696: rows listed past the head before the upgrade (no guard then) go
    /// with the backup, but `restore` drops them, so the entry it writes is
    /// sent at the next start.
    #[tokio::test]
    async fn a_planted_chain_seq_does_not_hide_a_later_restore() {
        use crate::maintenance::{archive, backup, restore};
        let Some(roles) = crate::db::scratch::Roles::create("a_planted_chain_seq_does_not_hide_a_later_restore").await
        else {
            return;
        };
        let a = roles.database().await;
        let b = roles.database().await;
        sqlx::query(
            "INSERT INTO cmdb.audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
             VALUES ('system', session_user, 'mfa.disable', 'users', gen_random_uuid(), '{}')",
        )
        .execute(&a.pool)
        .await
        .unwrap();
        let head_a = head(&a.pool).await.unwrap();
        // As the API role could on 0061: the guard is off for the plant.
        let mut owner = a.pool.acquire().await.unwrap();
        for sql in [
            "ALTER TABLE cmdb.audit_export_restores DISABLE TRIGGER audit_export_restores_guard",
            "INSERT INTO cmdb.audit_export_restores (chain_seq) SELECT generate_series(1, 300)",
            "ALTER TABLE cmdb.audit_export_restores ENABLE TRIGGER audit_export_restores_guard",
        ] {
            sqlx::query(sql).execute(&mut *owner).await.unwrap();
        }
        drop(owner);

        let api_a = roles.api_pool(&a).await;
        let mut buf = Vec::new();
        backup::write(&mut api_a.acquire().await.unwrap(), &mut buf, None).await.unwrap();
        let checked = archive::verify(buf.as_slice(), None).unwrap();
        let listed = checked.header.tables.iter().find(|t| t.name == "audit_export_restores").map(|t| t.rows);
        assert_eq!(listed, Some(300), "the backup holds the planted rows");

        let report = {
            let _one = crate::db::scratch::whole_schema_transaction().await;
            let mut cb = b.pool.acquire().await.unwrap();
            restore::restore(&mut cb, buf.as_slice(), &checked, true, true).await.unwrap()
        };
        assert_eq!(report.restored_head.chain_seq, head_a);
        assert_eq!(report.entry.chain_seq, head_a + 1);
        let left: i64 =
            sqlx::query_scalar("SELECT count(*) FROM cmdb.audit_export_restores").fetch_one(&b.pool).await.unwrap();
        assert_eq!(left, 0, "nothing listed matches a restore entry");

        let api_b = roles.api_pool(&b).await;
        let (cursor, unsent) = start(&api_b).await.unwrap();
        assert_eq!((cursor, unsent), (head_a, 1), "starts before the new entry");
        let (mut sink, path) = file_sink();
        let mut health = Health::default();
        assert_eq!(drain(&api_b, &mut sink, cursor, &mut health).await, report.entry.chain_seq);
        let out = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let sent: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(sent.len(), 1, "{out}");
        assert_eq!(
            (sent[0]["chainSeq"].as_i64(), sent[0]["action"].as_str()),
            (Some(head_a + 1), Some("backup.restore"))
        );
        assert!(health.failing.is_none());
        assert_eq!(start(&api_b).await.unwrap(), (head_a + 1, 0), "listed once sent");

        api_a.close().await;
        api_b.close().await;
        a.drop().await;
        b.drop().await;
        roles.drop().await;
    }

    /// GH#509: a refused sign-in under a name that matches no account (often a
    /// password typed into the wrong field) is exported without that name; a
    /// real account's name is kept, and the table keeps both.
    #[tokio::test]
    async fn unknown_attempted_usernames_are_not_exported() {
        let Some(db) = crate::db::scratch::database("unknown_attempted_usernames_are_not_exported").await else {
            return;
        };
        let start = head(&db.pool).await.unwrap();
        let ctx = crate::api::context::RequestContext::system("test", "req-509");
        let typed = "Tr0ub4dor&3-not-a-name";
        let mut conn = db.pool.acquire().await.unwrap();
        sqlx::query(
            "INSERT INTO users (username, display_name, password_hash) VALUES ('owner', 'Owner', '$argon2id$x')",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        let attempt = crate::auth::events::login_failure(&mut conn, &ctx, typed, None).await.unwrap();
        crate::auth::events::login_locked(&mut conn, &ctx, attempt, typed, Duration::from_secs(60)).await.unwrap();
        crate::auth::events::login_failure(&mut conn, &ctx, "OWNER", None).await.unwrap();
        drop(conn);

        // The stdout sink writes the same lines to another handle.
        let path = std::env::temp_dir().join(format!("shadoucmdb-audit-{}.jsonl", Uuid::new_v4()));
        let mut sink = Sink {
            target: AuditSink::File(path.clone()),
            format: AuditFormat::Json,
            facility: 13,
            hostname: "h".into(),
            tls: None,
            conn: None,
        };
        drain(&db.pool, &mut sink, start, &mut Health::default()).await;
        let out = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert!(!out.contains(typed), "{out}");
        let rows: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let logins: Vec<&Value> = rows.iter().filter(|r| r["action"].as_str().unwrap().starts_with("login.")).collect();
        assert_eq!(logins.len(), 3, "{out}");
        for unknown in &logins[..2] {
            assert_eq!(unknown["newValue"]["attemptedUsername"], Value::Null, "{unknown}");
            assert_eq!(unknown["newValue"]["attemptedUsernameRedacted"], true, "{unknown}");
        }
        assert_eq!(logins[1]["newValue"]["lockedForSeconds"], 60);
        assert_eq!(logins[2]["newValue"]["attemptedUsername"], "OWNER", "an account's name is kept");
        assert!(logins[2]["newValue"].get("attemptedUsernameRedacted").is_none());

        let stored: Vec<Value> = sqlx::query_scalar(
            "SELECT new_value FROM audit_log WHERE chain_seq > $1 AND action LIKE 'login.%' ORDER BY chain_seq",
        )
        .bind(start)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(stored[0]["attemptedUsername"], typed, "the table keeps the name as typed");
        db.drop().await;
    }
}
