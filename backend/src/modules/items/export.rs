//! `GET /configuration-items/export`: the inventory query (filters, sort,
//! columns) as a CSV file (SHAA-2353, requirement I8).
//!
//! - The rows are those `listConfigurationItems` returns for the same
//!   parameters, without a page: CIs of classes the caller may view only. A
//!   reference to a CI the caller may not view is left empty.
//! - Read from one snapshot (REPEATABLE READ, read only) through a cursor, a
//!   batch at a time, and streamed: memory does not grow with the inventory.
//! - Every field goes through [`csv_safe`]: quoted and neutralised.
//! - One `export` audit row (entity type `inventory`, nil id) with the
//!   parameters and the row count, never the rows, written before the first
//!   byte: a refused or failed start leaves no row, a started download does.
//! - Each download holds a database connection while the client reads, so
//!   they are capped per process and per user, a client that stops reading
//!   is cut off, and a download cut short ends with an error instead of
//!   looking complete.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::task::Poll;
use std::time::Duration;

use axum::body::Bytes;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use tokio::sync::mpsc;
use tokio::time::Instant;
use uuid::Uuid;

use super::plan::is_visible;
use super::schemas::{
    ActiveQuery, EXPORT_BUILTIN_COLUMNS, EXPORT_DEFAULT_COLUMNS, EXPORT_MAX_COLUMNS, ExportItemsQuery, ItemFilterQuery,
    KindQuery,
};
use super::service::{inventory_filters, list_sort};
use crate::api::context::RequestContext;
use crate::api::route::CsvDownload;
use crate::api::schemas::{Deleted, iso};
use crate::auth::permissions::ClassOp;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::items::{self as data, ItemValue, SummaryRow};
use crate::http::error::{AppError, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::modules::csv_safe;
use crate::modules::download_slots::Slots;
use crate::schema::model::Model;

/// CIs read from the cursor at a time.
const BATCH: u32 = 500;
/// Bytes collected before a piece of the file is sent.
const FLUSH_BYTES: usize = 64 * 1024;
/// Exports running at once in this process; more are answered 503 SERVER_BUSY.
pub const MAX_STREAMS: usize = 4;
/// Exports one user runs at once; more are answered 429 RATE_LIMITED.
pub const MAX_STREAMS_PER_USER: usize = 2;
/// How long a piece may wait for the client to read before the download is cut off.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one download may take in all.
const STREAM_DEADLINE: Duration = Duration::from_secs(30 * 60);

static STREAMS: LazyLock<Slots> = LazyLock::new(|| Slots::new(MAX_STREAMS, MAX_STREAMS_PER_USER, "inventory exports"));

const ATTRIBUTE_PREFIX: &str = "attributes.";

/// One column of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Column {
    /// One of [`EXPORT_BUILTIN_COLUMNS`]
    Builtin(&'static str),
    /// An attribute, by key
    Attribute(String),
}

impl Column {
    /// The header: the attribute key, or the built-in field in snake case, so
    /// that a bulk import maps the columns of an exported file by itself.
    fn header(&self) -> String {
        match self {
            Column::Builtin(name) => name.chars().fold(String::new(), |mut s, c| {
                if c.is_ascii_uppercase() {
                    s.push('_');
                    s.push(c.to_ascii_lowercase());
                } else {
                    s.push(c);
                }
                s
            }),
            Column::Attribute(key) => key.clone(),
        }
    }

    fn name(&self) -> String {
        match self {
            Column::Builtin(name) => (*name).to_owned(),
            Column::Attribute(key) => format!("{ATTRIBUTE_PREFIX}{key}"),
        }
    }
}

fn columns_error(message: String, code: &str) -> AppError {
    AppError::validation(vec![FieldError {
        location: FieldLocation::Query,
        field: "columns".into(),
        message,
        code: code.into(),
    }])
}

/// The `columns` parameter, checked: known names, each once, at most
/// [`EXPORT_MAX_COLUMNS`]; an attribute must be an active attribute (own or
/// inherited) of every class in `classId`, each a class the caller may view.
fn parse_columns(
    raw: Option<&str>,
    model: &Model,
    class_ids: Option<&[Uuid]>,
    visible: Option<&[Uuid]>,
) -> Result<Vec<Column>, AppError> {
    let Some(raw) = raw else {
        return Ok(EXPORT_DEFAULT_COLUMNS
            .iter()
            .filter_map(|c| EXPORT_BUILTIN_COLUMNS.iter().find(|b| *b == c))
            .map(|c| Column::Builtin(c))
            .collect());
    };
    let names: Vec<&str> = raw.split(',').map(str::trim).collect();
    if names.len() > EXPORT_MAX_COLUMNS {
        return Err(columns_error(format!("At most {EXPORT_MAX_COLUMNS} columns"), "too_big"));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if !seen.insert(name) {
            return Err(columns_error(format!("Column \"{name}\" is listed twice"), "duplicate"));
        }
        if let Some(b) = EXPORT_BUILTIN_COLUMNS.iter().find(|b| **b == name) {
            out.push(Column::Builtin(b));
            continue;
        }
        let key = match name.strip_prefix(ATTRIBUTE_PREFIX) {
            Some(key) if crate::schema::naming::is_identifier(key) => key,
            _ => {
                return Err(columns_error(
                    format!(
                        "Unknown column \"{name}\": one of {} or attributes.<key>",
                        EXPORT_BUILTIN_COLUMNS.join(", ")
                    ),
                    "unknown_column",
                ));
            }
        };
        let Some(class_ids) = class_ids else {
            return Err(columns_error(format!("The attribute column \"{name}\" needs classId"), "class_required"));
        };
        let on_every_class = class_ids.iter().all(|class_id| {
            is_visible(visible, *class_id)
                && model
                    .lineage(*class_id)
                    .into_iter()
                    .any(|c| model.own_fields(c.id).any(|f| f.key == key && f.is_active))
        });
        if !on_every_class {
            return Err(columns_error(
                format!("Attribute \"{key}\" is not an active attribute of every class in classId"),
                "unknown_attribute",
            ));
        }
        out.push(Column::Attribute(key.to_owned()));
    }
    Ok(out)
}

/// The lookup lists the attribute columns can hold values of.
fn lookup_lists(model: &Model, columns: &[Column]) -> Vec<Uuid> {
    let keys: HashSet<&str> = columns
        .iter()
        .filter_map(|c| match c {
            Column::Attribute(k) => Some(k.as_str()),
            Column::Builtin(_) => None,
        })
        .collect();
    let mut ids: Vec<Uuid> = model
        .fields
        .iter()
        .filter(|f| f.data_type == AttributeDataType::Lookup && keys.contains(f.key.as_str()))
        .filter_map(|f| f.lookup_list_id)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn bool_text(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

fn builtin_cell(row: &SummaryRow, name: &str) -> String {
    match name {
        "id" => row.id.to_string(),
        "label" => row.label.clone(),
        "ident" => row.ident.clone(),
        "class" => row.class_name.clone(),
        "criticality" => row.criticality_name.clone().unwrap_or_default(),
        "validFrom" => iso(&row.valid_from),
        "validUntil" => row.valid_until.as_ref().map(iso).unwrap_or_default(),
        "active" => bool_text(row.active).to_owned(),
        "createdAt" => iso(&row.created_at),
        "updatedAt" => iso(&row.updated_at),
        _ => String::new(),
    }
}

/// What the file shows for a stored value: the lookup value's name, the
/// referenced CI's label (empty when the caller may not view it), else the
/// value as the API returns it.
fn attribute_cell(
    v: &ItemValue,
    lookups: &HashMap<Uuid, String>,
    refs: &HashMap<Uuid, data::ReferencedItem>,
    visible: Option<&[Uuid]>,
) -> String {
    match v.data_type {
        AttributeDataType::Lookup => v
            .value
            .as_str()
            .and_then(|id| Uuid::parse_str(id).ok())
            .and_then(|id| lookups.get(&id).cloned())
            .unwrap_or_default(),
        AttributeDataType::Reference => v
            .reference()
            .and_then(|id| refs.get(&id))
            .filter(|r| is_visible(visible, r.class_id))
            .map(|r| r.label.clone())
            .unwrap_or_default(),
        _ => match &v.value {
            Value::String(s) => s.clone(),
            Value::Bool(b) => bool_text(*b).to_owned(),
            Value::Null => String::new(),
            other => other.to_string(),
        },
    }
}

/// What the export writes, besides the rows.
struct Layout {
    columns: Vec<Column>,
    delimiter: char,
    lookups: HashMap<Uuid, String>,
    visible: Option<Vec<Uuid>>,
}

impl Layout {
    fn header_record(&self, out: &mut String) {
        let headers: Vec<String> = self.columns.iter().map(Column::header).collect();
        csv_safe::write_record(out, self.delimiter, headers.iter().map(String::as_str));
    }

    /// Appends one batch of CIs.
    async fn rows(
        &self,
        conn: &mut PgConnection,
        model: &Model,
        rows: &[SummaryRow],
        out: &mut String,
    ) -> sqlx::Result<()> {
        let wanted: HashSet<&str> = self
            .columns
            .iter()
            .filter_map(|c| match c {
                Column::Attribute(k) => Some(k.as_str()),
                Column::Builtin(_) => None,
            })
            .collect();
        let mut by_ci: HashMap<Uuid, HashMap<String, ItemValue>> = HashMap::new();
        if !wanted.is_empty() {
            let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
            for v in data::values(conn, model, &ids).await? {
                if wanted.contains(v.key.as_str()) {
                    by_ci.entry(v.ci_id).or_default().insert(v.key.clone(), v);
                }
            }
        }
        let ref_ids: Vec<Uuid> = by_ci.values().flat_map(|m| m.values()).filter_map(ItemValue::reference).collect();
        let refs = data::reference_names(conn, &ref_ids).await?;
        let none = HashMap::new();
        for row in rows {
            let values = by_ci.get(&row.id).unwrap_or(&none);
            let cells: Vec<String> = self
                .columns
                .iter()
                .map(|c| match c {
                    Column::Builtin(name) => builtin_cell(row, name),
                    Column::Attribute(key) => values
                        .get(key)
                        .map(|v| attribute_cell(v, &self.lookups, &refs, self.visible.as_deref()))
                        .unwrap_or_default(),
                })
                .collect();
            csv_safe::write_record(out, self.delimiter, cells.iter().map(String::as_str));
        }
        Ok(())
    }
}

/// Where the writer sends the pieces: each send waits at most `SEND_TIMEOUT`
/// and all of them together at most until the deadline.
struct Sink {
    tx: mpsc::Sender<Result<Bytes, std::io::Error>>,
    deadline: Instant,
    timeout: Duration,
    /// Set when the file was not written to the end; the body then ends with
    /// an error instead of looking complete.
    failed: Arc<AtomicBool>,
}

impl Sink {
    /// `false` once the client is gone or too slow; the writer stops then.
    async fn send(&self, piece: Bytes) -> bool {
        let wait = self.deadline.saturating_duration_since(Instant::now()).min(self.timeout);
        match tokio::time::timeout(wait, self.tx.send(Ok(piece))).await {
            Ok(Ok(())) => true,
            Ok(Err(_)) => false,
            Err(_) => {
                tracing::warn!("inventory export: the client did not read the download in time; stopped");
                self.fail();
                false
            }
        }
    }

    fn fail(&self) {
        self.failed.store(true, Ordering::SeqCst);
    }
}

/// Reads the cursor batch by batch into `sink`. `Ok` also when the client went away.
async fn write(conn: &mut PgConnection, model: &Model, layout: &Layout, sink: &Sink) -> sqlx::Result<()> {
    let mut out = String::from(csv_safe::BOM);
    layout.header_record(&mut out);
    loop {
        let rows = data::fetch_list_cursor(conn, BATCH).await?;
        let last = (rows.len() as u32) < BATCH;
        layout.rows(conn, model, &rows, &mut out).await?;
        if (last || out.len() >= FLUSH_BYTES)
            && !out.is_empty()
            && !sink.send(Bytes::from(std::mem::take(&mut out))).await
        {
            return Ok(());
        }
        if last {
            return Ok(());
        }
    }
}

fn active_text(a: ActiveQuery) -> &'static str {
    match a {
        ActiveQuery::True => "true",
        ActiveQuery::False => "false",
        ActiveQuery::All => "all",
    }
}

fn deleted_text(d: Deleted) -> &'static str {
    match d {
        Deleted::Exclude => "exclude",
        Deleted::Include => "include",
        Deleted::Only => "only",
    }
}

fn kind_text(k: KindQuery) -> &'static str {
    match k {
        KindQuery::Asset => "asset",
        KindQuery::Process => "process",
        KindQuery::Any => "any",
    }
}

/// The query parameters as the audit row records them: the ones sent or in
/// effect, by id as in the request.
fn audit_query(q: &ExportItemsQuery) -> Value {
    let ids = |l: Option<&crate::api::schemas::UuidList>| l.map(|l| json!(l.0));
    let mut filters = serde_json::Map::new();
    let mut put = |k: &str, v: Option<Value>| {
        if let Some(v) = v {
            filters.insert(k.into(), v);
        }
    };
    put("q", q.q.as_ref().map(|s| json!(s)));
    put("classId", ids(q.class_id.as_ref()));
    put("includeSubclasses", Some(json!(bool::from(q.include_subclasses))));
    put("active", Some(json!(active_text(q.active))));
    put("lookupValueId", ids(q.lookup_value_id.as_ref()));
    put("ipWithin", q.ip_within.as_ref().map(|s| json!(s)));
    put("criticalityValueId", ids(q.criticality_value_id.as_ref()));
    put("deleted", Some(json!(deleted_text(q.deleted))));
    put("ownLayout", q.own_layout.map(|b| json!(bool::from(b))));
    put("layoutTemplate", q.layout_template.as_ref().map(|s| json!(s)));
    put("kind", q.kind.map(|k| json!(kind_text(k))));
    put("businessServiceId", ids(q.business_service_id.as_ref()));
    Value::Object(filters)
}

/// `inventory-<UTC timestamp>.csv`
fn file_name() -> String {
    format!("inventory-{}.csv", Utc::now().format("%Y%m%d-%H%M%S"))
}

/// The export: checks the parameters, records the audit row, then streams.
pub async fn export(pool: &PgPool, ctx: &RequestContext, q: &ExportItemsQuery) -> Result<CsvDownload, AppError> {
    // Before anything is read: a refused download costs nothing.
    let slot = STREAMS.acquire(ctx.principal().map(|p| p.user_id))?;

    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
    let model = Model::load(&mut tx).await?;
    let own_layout = q.own_layout.map(bool::from);
    let f =
        inventory_filters(&mut tx, ctx, &model, q, q.q.as_deref(), own_layout, q.layout_template.as_deref()).await?;
    let visible = ctx.class_scope(ClassOp::View);
    let columns =
        parse_columns(q.columns.as_deref(), &model, q.class_id().map(|l| l.0.as_slice()), visible.as_deref())?;
    let total = {
        let sort = list_sort(&model, &q.sort, q.class_id())?;
        let total = data::count(&mut tx, &f).await?;
        data::declare_list_cursor(&mut tx, &f, &sort, q.sort.desc).await?;
        total
    };
    let lookups = data::lookup_value_names(&mut tx, &lookup_lists(&model, &columns)).await?;

    // GH#514: the snapshot is read-only; the audit row goes in its own
    // transaction, so an audited write committed meanwhile cannot make the
    // chain-head update a serialization failure.
    let entry = AuditEntry {
        action: AuditAction::Export,
        entity_type: "inventory",
        entity_id: Uuid::nil(),
        old_value: None,
        new_value: Some(json!({
            "kind": "inventory",
            "format": "csv",
            "columns": columns.iter().map(Column::name).collect::<Vec<_>>(),
            "sort": format!("{}{}", if q.sort.desc { "-" } else { "" }, q.sort.field),
            "filters": audit_query(q),
            "rowCount": total,
            "visibility": if visible.is_some() { "restricted" } else { "all_classes" },
        })),
    };
    let mut audit = pool.begin().await?;
    crud::write_audit(&mut audit, ctx, vec![entry]).await?;
    audit.commit().await?;

    let layout = Layout { columns, delimiter: q.delimiter.char(), lookups, visible };
    let (tx_pieces, mut rx) = mpsc::channel(4);
    let sink = Sink {
        tx: tx_pieces,
        deadline: Instant::now() + STREAM_DEADLINE,
        timeout: SEND_TIMEOUT,
        failed: Arc::default(),
    };
    let failed = sink.failed.clone();
    tokio::spawn(async move {
        let _slot = slot;
        if let Err(e) = write(&mut tx, &model, &layout, &sink).await {
            tracing::warn!(error = %e, "inventory export: reading the inventory failed; the download was cut short");
            sink.fail();
        }
        // Read only: nothing to commit; the cursor closes with the transaction.
        let _ = tx.rollback().await;
    });
    let mut ended = false;
    let body = axum::body::Body::from_stream(futures_util::stream::poll_fn(move |cx| match rx.poll_recv(cx) {
        Poll::Ready(None) if !ended && failed.load(Ordering::SeqCst) => {
            ended = true;
            Poll::Ready(Some(Err(std::io::Error::other("the inventory export was not written to the end"))))
        }
        other => other,
    }));
    Ok(CsvDownload { file_name: file_name(), body })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_headers_are_snake_case() {
        let headers: Vec<String> = EXPORT_BUILTIN_COLUMNS.iter().map(|c| Column::Builtin(c).header()).collect();
        assert_eq!(
            headers,
            [
                "id",
                "label",
                "ident",
                "class",
                "criticality",
                "valid_from",
                "valid_until",
                "active",
                "created_at",
                "updated_at"
            ]
        );
        assert_eq!(Column::Attribute("os_family".into()).header(), "os_family");
    }

    #[test]
    fn columns_are_checked() {
        let model = Model::default();
        let parse = |raw: Option<&str>| parse_columns(raw, &model, None, None);
        let code = |raw: &str| parse(Some(raw)).err().and_then(|e| e.details).map(|d| d[0].code.clone());
        assert_eq!(parse(None).unwrap().len(), EXPORT_DEFAULT_COLUMNS.len());
        assert_eq!(parse(Some("ident, label")).unwrap(), [Column::Builtin("ident"), Column::Builtin("label")]);
        assert_eq!(code("label,label").as_deref(), Some("duplicate"));
        assert_eq!(code("className").as_deref(), Some("unknown_column"));
        assert_eq!(code("attributes.Bad-Key").as_deref(), Some("unknown_column"));
        assert_eq!(code("attributes.hostname").as_deref(), Some("class_required"));
        let many = vec!["label"; EXPORT_MAX_COLUMNS + 1].join(",");
        assert_eq!(code(&many).as_deref(), Some("too_big"));
        // A class the caller may not view has no attributes for them.
        let class = Uuid::new_v4();
        let err = parse_columns(Some("attributes.hostname"), &model, Some(&[class]), Some(&[])).unwrap_err();
        assert_eq!(err.details.unwrap()[0].code, "unknown_attribute");
    }
}
