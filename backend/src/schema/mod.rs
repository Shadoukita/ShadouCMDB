//! The DDL engine: keeps the physical schema in step with the data model.
//!
//! Areas are PostgreSQL schemas, types are tables and fields are columns (see
//! migration 0008/0009). Services change the metadata rows (cmdb.areas,
//! cmdb.ci_classes, cmdb.ci_attribute_definitions) and then call [`apply`] in
//! the same transaction. The engine
//!
//! 1. takes the schema advisory lock, so two administrators never change the
//!    schema at the same time (the second waits for the first to commit);
//! 2. compares the metadata with the catalog and builds a plan: the DDL
//!    statements plus their impact on existing data;
//! 3. runs the data-loss guards: a field type change is dry-run against every
//!    stored value and refused if any would not convert, a field is made
//!    NOT NULL only when no asset lacks a value, enum values still in use cannot
//!    be removed; nothing is ever dropped unless the caller purges it;
//! 4. executes the statements (all or nothing: they run in the caller's
//!    transaction), rebuilds the affected reporting views, and records the plan
//!    in cmdb.schema_changes and the audit log.
//!
//! Every identifier comes from a validated technical name ([`naming`]) and is
//! quoted; the only literals are enum values, quoted by PostgreSQL itself
//! (`format('%L')`), and hex digests. No user text is ever spliced into SQL.
//!
//! A preview runs the same code in a transaction that is rolled back;
//! [`collect_previews`] captures the plans that would have been applied.

pub mod catalog;
pub mod model;
pub mod naming;

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{AssertSqlSafe, PgConnection};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::context::{RequestContext, forbidden};
use crate::api::schemas::ts;
use crate::auth::permissions::ClassOp;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use catalog::{Catalog, REPORTING_ROLE};
use model::{Field, Model, TableName, pg_type};
use naming::{Ident, quote_ident};

/// Serialises every schema change (and the data model writes that lead to one).
pub async fn lock(conn: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('shadoucmdb:schema'))").execute(&mut *conn).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Plans
// ---------------------------------------------------------------------------

/// What a statement does to data that already exists
#[derive(Debug, Clone, Serialize, serde::Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Impact {
    /// Index into `statements` this applies to; null for the plan as a whole
    #[schema(required = true)]
    pub statement: Option<usize>,
    /// create, add_column, rewrite, not_null, drop_column, drop_table, drop_schema, warning, data_moved, ...
    pub kind: String,
    /// Rows (assets) concerned, when known and the caller may view them all
    #[schema(required = true)]
    pub rows: Option<i64>,
    pub message: String,
}

/// One applied schema change: the exact DDL, in order, and its impact
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SchemaChange {
    pub id: Uuid,
    #[serde(serialize_with = "ts::serialize")]
    pub occurred_at: DateTime<Utc>,
    pub actor_type: String,
    #[schema(required = true)]
    pub actor_id: Option<String>,
    #[schema(required = true)]
    pub actor_name: Option<String>,
    #[schema(required = true)]
    pub request_id: Option<String>,
    pub summary: String,
    pub statements: Vec<String>,
    #[schema(value_type = Vec<Impact>)]
    pub impact: sqlx::types::Json<Vec<Impact>>,
}

/// The one-line summary of a schema change, and the same line without the
/// counts it tells (for readers who may not view the types counted, GH#252).
#[derive(Debug, Clone)]
pub struct Summary {
    pub text: String,
    /// Set when `text` carries counts of the purged types' data ([`Purge::classes`]).
    pub without_counts: Option<String>,
}

impl Summary {
    pub fn counted(text: String, without_counts: String) -> Self {
        Self { text, without_counts: Some(without_counts) }
    }
}

impl From<&str> for Summary {
    fn from(text: &str) -> Self {
        Self { text: text.to_owned(), without_counts: None }
    }
}

impl From<&String> for Summary {
    fn from(text: &String) -> Self {
        text.as_str().into()
    }
}

/// Which types to compare with the catalog.
#[derive(Debug, Clone)]
pub enum Scope {
    /// Every area, type and field (template install, import, `migrate`).
    All,
    /// These types (their fields, and the views of everything below them).
    Classes(Vec<Uuid>),
    /// Only the area schemas.
    Areas,
}

/// Objects to drop: only ever what an administrator purged.
#[derive(Debug, Clone, Default)]
pub struct Purge {
    pub schemas: Vec<Ident>,
    pub tables: Vec<TableName>,
    /// (table, column)
    pub columns: Vec<(TableName, Ident)>,
    /// The types whose assets' values the tables and columns hold (collected
    /// before their definitions are deleted). How many values a purge deletes
    /// is only told to a caller who may view them all (GH#243).
    pub classes: Vec<Uuid>,
    /// Rows of `tables` counted before the purge emptied them in the same
    /// transaction; a table not listed is counted when the plan is built (GH#281).
    pub rows: HashMap<TableName, i64>,
}

impl Purge {
    /// Whether a caller who may view `visible` (`None`: every type) may learn how much the purge deletes.
    pub fn reveals(&self, visible: Option<&[Uuid]>) -> bool {
        visible.is_none_or(|v| self.classes.iter().all(|id| v.contains(id)))
    }
}

#[derive(Default)]
struct Plan {
    /// Views dropped before the table changes that they depend on.
    pre: Vec<String>,
    ddl: Vec<String>,
    /// Views (re)created and grants, after the table changes.
    post: Vec<String>,
    /// (index into ddl, impact, its message without the count when it tells one)
    impact: Vec<(Option<usize>, Impact, Option<String>)>,
    /// The types (with every type below them) whose stored data the counts describe.
    counted: BTreeSet<Uuid>,
    /// Tables whose dependent views must be rebuilt (a column type changed or was dropped).
    rebuild: HashSet<TableName>,
}

impl Plan {
    fn ddl(&mut self, sql: String) -> usize {
        self.ddl.push(sql);
        self.ddl.len() - 1
    }

    fn note(&mut self, statement: Option<usize>, kind: &str, rows: Option<i64>, message: String) {
        self.impact.push((statement, Impact { statement: None, kind: kind.into(), rows, message }, None));
    }

    /// A note that counts stored data of `classes`; `without` is its message
    /// for a reader who may not view them all.
    fn counted(
        &mut self,
        statement: Option<usize>,
        kind: &str,
        rows: i64,
        message: String,
        without: String,
        classes: &[Uuid],
    ) {
        self.impact.push((
            statement,
            Impact { statement: None, kind: kind.into(), rows: Some(rows), message },
            Some(without),
        ));
        self.counted.extend(classes);
    }

    fn is_empty(&self) -> bool {
        self.pre.is_empty() && self.ddl.is_empty() && self.post.is_empty()
    }

    fn statements(&self) -> Vec<String> {
        self.pre.iter().chain(&self.ddl).chain(&self.post).cloned().collect()
    }

    fn impacts(&self) -> Vec<Impact> {
        let offset = self.pre.len();
        self.impact.iter().map(|(i, imp, _)| Impact { statement: i.map(|i| i + offset), ..imp.clone() }).collect()
    }

    /// The impact without any count (None: it tells none).
    fn redacted_impacts(&self) -> Option<Vec<Impact>> {
        self.impact.iter().any(|(_, _, without)| without.is_some()).then(|| {
            self.impacts()
                .into_iter()
                .zip(&self.impact)
                .map(|(imp, (_, _, without))| match without {
                    Some(message) => Impact { rows: None, message: message.clone(), ..imp },
                    None => imp,
                })
                .collect()
        })
    }
}

/// 422 SCHEMA_CHANGE_REFUSED naming the request field whose change was refused.
pub fn refused(field: &str, code: &str, message: String) -> AppError {
    AppError::new(ErrorCode::SchemaChangeRefused, message.clone()).with_details(vec![FieldError {
        location: FieldLocation::Body,
        field: field.into(),
        message,
        code: code.into(),
    }])
}

/// 403 `view_required` for a change whose outcome depends on stored assets the
/// caller may not view: refused before any of them is read.
pub fn view_required(field: &str, message: String) -> AppError {
    forbidden(message.clone()).with_details(vec![FieldError {
        location: FieldLocation::Body,
        field: field.into(),
        message,
        code: "view_required".into(),
    }])
}

/// The types whose CIs the caller may learn about (`None`: every type): those
/// they may view, and those created in this transaction, which hold no CIs
/// anyone could have stored before (a config import creates a type and makes
/// its fields required in one go).
pub async fn visible_classes(conn: &mut PgConnection, ctx: &RequestContext) -> Result<Option<Vec<Uuid>>, AppError> {
    let Some(mut visible) = ctx.class_scope(ClassOp::View) else { return Ok(None) };
    let new: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM cmdb.ci_classes WHERE created_at = now()").fetch_all(&mut *conn).await?;
    visible.extend(new);
    Ok(Some(visible))
}

/// Whether the caller may learn about the CIs of every type in `classes`.
pub async fn may_view_all(conn: &mut PgConnection, ctx: &RequestContext, classes: &[Uuid]) -> Result<bool, AppError> {
    Ok(visible_classes(conn, ctx).await?.is_none_or(|v| classes.iter().all(|id| v.contains(id))))
}

/// 422 INVALID_NAME for a technical name.
pub fn invalid_name(field: &str, code: &str, message: String) -> AppError {
    AppError::new(ErrorCode::InvalidName, message.clone()).with_details(vec![FieldError {
        location: FieldLocation::Body,
        field: field.into(),
        message,
        code: code.into(),
    }])
}

/// A technical name about to be stored: 422 INVALID_NAME with the reason, or Ok.
pub fn validate_name(name: &str, kind: naming::NameKind, field: &str) -> Result<(), AppError> {
    naming::validate(name, kind).map_err(|p| {
        let article = if kind == naming::NameKind::Area { "an" } else { "a" };
        invalid_name(
            field,
            p.code,
            format!("Technical name \"{name}\" cannot be used for {article} {}: {}", kind.as_str(), p.message),
        )
    })
}

/// The `key` a create body sets, as text.
pub fn key_column(columns: &crate::data::crud::ColumnSet) -> Option<&str> {
    columns.0.iter().find_map(|(c, v)| match (c, v) {
        (&"key", crate::data::crud::Val::Text(Some(k))) => Some(k.as_str()),
        _ => None,
    })
}

fn field_error(field: &str, code: &str, message: String) -> AppError {
    refused(field, code, message)
}

// ---------------------------------------------------------------------------
// Names derived from ids (constraints and indexes never carry user text)
// ---------------------------------------------------------------------------

fn enum_hash(values: &[String]) -> String {
    hex::encode(Sha256::digest(values.join("\n").as_bytes()))[..12].to_owned()
}

fn check_name(f: &Field) -> String {
    format!("ck_{}_{}", f.hex(), enum_hash(f.enum_list()))
}

fn fk_name(f: &Field) -> String {
    format!("fk_{}", f.hex())
}

fn index_name(f: &Field) -> String {
    format!("ix_{}", f.hex())
}

/// The unique index of the Person's Email (cmdb.email_key(email), migrations 0044 and 0045).
pub fn unique_index_name(f: &Field) -> String {
    format!("uq_{}", f.hex())
}

fn fk_target(t: AttributeDataType) -> Option<&'static str> {
    match t {
        AttributeDataType::Reference => Some("cmdb.configuration_items"),
        AttributeDataType::Lookup => Some("cmdb.lookup_list_values"),
        _ => None,
    }
}

/// References are checked at the end of the statement (a purge deletes CIs
/// that reference each other); a lookup value in use cannot be deleted.
fn fk_action(t: AttributeDataType) -> &'static str {
    match t {
        AttributeDataType::Reference => "NO ACTION",
        _ => "RESTRICT",
    }
}

/// The column as text, the way the API shows it (an inet without its /32).
fn text_expr(column: &Ident, pg_type: &str) -> String {
    match pg_type {
        "inet" => format!("abbrev({column})"),
        _ => format!("{column}::text"),
    }
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

async fn count(conn: &mut PgConnection, sql: String) -> Result<i64, AppError> {
    Ok(sqlx::query_scalar::<_, i64>(AssertSqlSafe(sql)).persistent(false).fetch_one(&mut *conn).await?)
}

async fn samples(conn: &mut PgConnection, sql: String) -> Result<Vec<String>, AppError> {
    Ok(sqlx::query_scalar::<_, String>(AssertSqlSafe(sql)).persistent(false).fetch_all(&mut *conn).await?)
}

/// Stored values (as text) that are not in the allowed list: their count, and
/// up to five of them.
async fn values_outside(
    conn: &mut PgConnection,
    table: &TableName,
    expr: &str,
    column: &Ident,
    allowed: &[String],
) -> Result<(i64, Vec<String>), AppError> {
    let base = format!("FROM {} WHERE {column} IS NOT NULL AND NOT ({expr} = ANY($1))", table.sql());
    let n: i64 = sqlx::query_scalar(AssertSqlSafe(format!("SELECT count(*) {base}")))
        .bind(allowed)
        .persistent(false)
        .fetch_one(&mut *conn)
        .await?;
    let sample: Vec<String> = if n > 0 {
        sqlx::query_scalar(AssertSqlSafe(format!("SELECT DISTINCT {expr} {base} ORDER BY 1 LIMIT 5")))
            .bind(allowed)
            .persistent(false)
            .fetch_all(&mut *conn)
            .await?
    } else {
        Vec::new()
    };
    Ok((n, sample))
}

fn quoted_list(values: &[String]) -> String {
    values.iter().map(|v| format!("\"{v}\"")).collect::<Vec<_>>().join(", ")
}

/// `: "a", "b"` for a refusal message.
fn listed(values: &[String]) -> String {
    format!(": {}", quoted_list(values))
}

/// What the dry run looks for: values that would not convert at all, or
/// values that would convert but lose part of themselves (a narrowing change).
enum Guard {
    Castable(String),
    Lossless(String),
}

/// `ALTER COLUMN .. TYPE .. USING ..` for a type change, after a dry run of the
/// conversion over every stored value. A change must be lossless or it is refused
/// with up to five offending values. `classes`: the types whose assets the table holds.
async fn type_change(
    conn: &mut PgConnection,
    plan: &mut Plan,
    table: &TableName,
    f: &Field,
    from: &str,
    classes: &[Uuid],
) -> Result<(), AppError> {
    let col = f.column();
    let to = pg_type(f.data_type);
    let rows = count(conn, format!("SELECT count(*) FROM {} WHERE {col} IS NOT NULL", table.sql())).await?;
    let (using, bad): (String, Option<Guard>) = match (from, to) {
        (_, "text") => (text_expr(&col, from), None),
        ("bigint", "numeric") => (format!("{col}::numeric"), None),
        ("numeric", "bigint") => (
            format!("{col}::bigint"),
            Some(Guard::Castable(format!(
                "{col} <> trunc({col}) OR {col} NOT BETWEEN -9223372036854775808 AND 9223372036854775807"
            ))),
        ),
        // Dates are UTC days: never through text, whose meaning depends on the session TimeZone.
        ("timestamp with time zone", "date") => (
            format!("({col} AT TIME ZONE 'UTC')::date"),
            Some(Guard::Lossless(format!("({col} AT TIME ZONE 'UTC')::time <> '00:00'"))),
        ),
        ("date", "timestamp with time zone") => (format!("{col}::timestamp AT TIME ZONE 'UTC'"), None),
        ("uuid", _) | (_, "uuid") => {
            if rows > 0 {
                return Err(field_error(
                    "dataType",
                    "type_change_unsupported",
                    format!(
                        "{} assets hold a value for \"{}\"; reference and lookup fields cannot change type while they \
                         hold values. Clear them first.",
                        rows, f.key
                    ),
                ));
            }
            (format!("NULL::{to}"), None)
        }
        _ => {
            let text = text_expr(&col, from);
            (
                format!("({text})::{to}"),
                Some(Guard::Castable(format!("NOT cmdb.value_castable({text}, '{to}'::regtype)"))),
            )
        }
    };
    if let Some(guard) = bad {
        let (Guard::Castable(bad) | Guard::Lossless(bad)) = &guard;
        let where_bad = format!("FROM {} WHERE {col} IS NOT NULL AND ({bad})", table.sql());
        let failing = count(conn, format!("SELECT count(*) {where_bad}")).await?;
        if failing > 0 {
            let shown = match from {
                "timestamp with time zone" => {
                    format!("to_char({col} AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')")
                }
                _ => text_expr(&col, from),
            };
            let sample = samples(conn, format!("SELECT DISTINCT {shown} {where_bad} ORDER BY 1 LIMIT 5")).await?;
            let (code, message) = match guard {
                Guard::Castable(_) => (
                    "type_change_failed",
                    format!(
                        "{failing} of {rows} stored values of \"{}\" cannot be converted to {}{}. Correct or clear \
                         them first; nothing was changed.",
                        f.key,
                        f.data_type.as_str(),
                        listed(&sample)
                    ),
                ),
                Guard::Lossless(_) => (
                    "type_change_lossy",
                    format!(
                        "{failing} of {rows} stored values of \"{}\" would lose information as {}{}. Correct or \
                         clear them first (a date holds no time of day: only values at midnight UTC convert); nothing \
                         was changed.",
                        f.key,
                        f.data_type.as_str(),
                        listed(&sample)
                    ),
                ),
            };
            return Err(field_error("dataType", code, message));
        }
    }
    let i = plan.ddl(format!("ALTER TABLE {} ALTER COLUMN {col} TYPE {to} USING {using}", table.sql()));
    let converted = format!("{}.{} converted from {from} to {to} (dry run: all convert)", table.display(), f.key);
    plan.counted(
        Some(i),
        "rewrite",
        rows,
        format!("{rows} values of {converted}"),
        format!("The values of {converted}"),
        classes,
    );
    plan.rebuild.insert(table.clone());
    Ok(())
}

// ---------------------------------------------------------------------------
// Planning
// ---------------------------------------------------------------------------

struct Planner<'a> {
    model: &'a Model,
    catalog: &'a Catalog,
    /// NOT NULL that cannot be applied yet is a warning rather than an error (full reconcile).
    lenient_not_null: bool,
    /// Classes the caller may view (`None`: every class). A change whose outcome
    /// depends on stored values needs view on every asset of the table.
    visible: Option<&'a [Uuid]>,
}

impl Planner<'_> {
    /// Whether the caller may learn about the stored values of `f`: its table holds
    /// the assets of its type and of every type below it (deleted ones included).
    fn reveals(&self, f: &Field) -> bool {
        self.visible.is_none_or(|v| self.model.subtree(f.class_id).iter().all(|id| v.contains(id)))
    }

    /// 403 for a change whose outcome depends on the stored values of `f` (a type
    /// change, a new enum list) when the caller may not view them all. Refused
    /// before any value is read: success, refusal and counts would all tell a
    /// caller which values are stored (GH#180, GH#221).
    fn may_check_values(&self, f: &Field, field: &str) -> Result<(), AppError> {
        if self.reveals(f) {
            return Ok(());
        }
        Err(view_required(
            field,
            format!(
                "Changing \"{}\" this way is checked against the values its assets store; that needs the view right \
                 on its type and every type below it. Nothing was changed.",
                f.key
            ),
        ))
    }

    async fn field(
        &self,
        conn: &mut PgConnection,
        plan: &mut Plan,
        table: &TableName,
        f: &Field,
    ) -> Result<(), AppError> {
        let col = f.column();
        let to = pg_type(f.data_type);
        let schema = table.schema.as_str();
        let existing = self.catalog.column(table, &f.key);
        let is_new = existing.is_none();
        let current_type = existing.map(|c| c.data_type.clone()).unwrap_or_else(|| to.to_owned());
        let constraints = self.catalog.constraints(table);
        let own = |prefix: &str| {
            let p = format!("{prefix}_{}", f.hex());
            constraints.iter().filter(move |c| c.name == p || c.name.starts_with(&format!("{p}_"))).collect::<Vec<_>>()
        };

        // Constraints that no longer fit go first (a type change would trip over them).
        let target = fk_target(f.data_type);
        for c in own("fk") {
            if target.is_none() || c.references.as_deref() != target {
                plan.ddl(format!("ALTER TABLE {} DROP CONSTRAINT {}", table.sql(), quote_ident(&c.name)));
            }
        }
        if target.is_none() && self.catalog.has_index(schema, &index_name(f)) {
            plan.ddl(format!("DROP INDEX {}.{}", table.schema, quote_ident(&index_name(f))));
        }
        let wanted_check = (f.data_type == AttributeDataType::Enum).then(|| check_name(f));
        let checks = own("ck");
        for c in &checks {
            if Some(&c.name) != wanted_check.as_ref() {
                plan.ddl(format!("ALTER TABLE {} DROP CONSTRAINT {}", table.sql(), quote_ident(&c.name)));
            }
        }

        if is_new {
            let i = plan.ddl(format!("ALTER TABLE {} ADD COLUMN {col} {to}", table.sql()));
            plan.note(Some(i), "add_column", None, format!("New column {}.{} ({to})", table.display(), f.key));
        } else if current_type != to {
            self.may_check_values(f, "dataType")?;
            type_change(conn, plan, table, f, &current_type, &self.model.subtree(f.class_id)).await?;
        }

        // Enum: the CHECK carries a hash of the allowed values, so a changed list gets a new constraint.
        if let Some(name) = &wanted_check
            && !checks.iter().any(|c| &c.name == name)
        {
            if !is_new {
                // No check yet: the field is becoming an enum; otherwise its list changed.
                let field = if checks.is_empty() { "dataType" } else { "enumValues" };
                self.may_check_values(f, field)?;
                let expr = text_expr(&col, &current_type);
                let (n, sample) = values_outside(conn, table, &expr, &col, f.enum_list()).await?;
                if n > 0 {
                    return Err(field_error(
                        field,
                        "enum_value_in_use",
                        format!(
                            "{n} assets store values of \"{}\" that are not in the list{}. Add them to the list or \
                             change those assets first.",
                            f.key,
                            listed(&sample)
                        ),
                    ));
                }
            }
            let list: String =
                sqlx::query_scalar("SELECT format('%L', $1::text[])").bind(f.enum_list()).fetch_one(&mut *conn).await?;
            plan.ddl(format!(
                "ALTER TABLE {} ADD CONSTRAINT {} CHECK ({col} = ANY ({list}::text[]))",
                table.sql(),
                quote_ident(name)
            ));
        }

        // Reference and lookup: a foreign key and its index.
        if let Some(target) = target {
            if !own("fk").iter().any(|c| c.references.as_deref() == Some(target)) {
                plan.ddl(format!(
                    "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({col}) REFERENCES {target} (id) ON DELETE {}",
                    table.sql(),
                    quote_ident(&fk_name(f)),
                    fk_action(f.data_type)
                ));
            }
            if !self.catalog.has_index(schema, &index_name(f)) {
                plan.ddl(format!("CREATE INDEX {} ON {} ({col})", quote_ident(&index_name(f)), table.sql()));
            }
        }

        // The Person's Email is unique ignoring case and Unicode form (SHAA-1505
        // decision 2, GH#531): the database holds the line for every write path.
        // Built before any Person exists (the type is new in 0044), so nothing can
        // violate it then; a later build finds duplicates and is refused with the
        // index's error. On a database between 0044 and 0045 (`cmdb.email_key`
        // is new in 0045) it is built on lower(), as 0044 did; 0045 rebuilds it.
        if f.is_unique_email() && !self.catalog.has_index(schema, &unique_index_name(f)) {
            let (key, compared) = if self.catalog.email_key {
                (format!("cmdb.email_key({col})"), "case and Unicode form")
            } else {
                (format!("lower({col})"), "case")
            };
            let i = plan.ddl(format!(
                "CREATE UNIQUE INDEX {} ON {} ({key})",
                quote_ident(&unique_index_name(f)),
                table.sql()
            ));
            plan.note(
                Some(i),
                "unique_index",
                None,
                format!("{}.{} is unique ignoring {compared}", table.display(), f.key),
            );
        }

        // NOT NULL for a required, active field; refused while any asset has no value.
        let is_not_null = existing.is_some_and(|c| c.not_null);
        if f.not_null() && !is_not_null {
            // Success or refusal would tell whether any asset lacks a value (GH#267).
            if self.catalog.has_table(table) && !self.reveals(f) {
                if !self.lenient_not_null {
                    self.may_check_values(f, "isRequired")?;
                }
                // Lenient (template install): the field stays nullable without
                // counting, whether or not any asset lacks a value (GH#276).
                plan.note(
                    None,
                    "warning",
                    None,
                    format!(
                        "{}.{} stays nullable: it may only become required by someone with the view right on its type \
                         and every type below it",
                        table.display(),
                        f.key
                    ),
                );
                return Ok(());
            }
            let nulls = if !self.catalog.has_table(table) {
                0
            } else if is_new {
                count(conn, format!("SELECT count(*) FROM {}", table.sql())).await?
            } else {
                count(conn, format!("SELECT count(*) FROM {} WHERE {col} IS NULL", table.sql())).await?
            };
            if nulls == 0 {
                let i = plan.ddl(format!("ALTER TABLE {} ALTER COLUMN {col} SET NOT NULL", table.sql()));
                plan.note(Some(i), "not_null", Some(0), format!("{}.{} becomes required", table.display(), f.key));
            } else if self.lenient_not_null {
                let field = format!("{}.{} stays nullable", table.display(), f.key);
                let without = format!("{field}: some assets have no value");
                if self.reveals(f) {
                    let message = format!("{field}: {nulls} assets have no value");
                    plan.counted(None, "warning", nulls, message, without, &self.model.subtree(f.class_id));
                } else {
                    plan.note(None, "warning", None, without);
                }
            } else {
                return Err(field_error(
                    "isRequired",
                    "values_missing",
                    format!(
                        "{nulls} assets (deleted ones included) have no value for \"{}\". Fill it in on those assets \
                         first, or keep the field optional.",
                        f.key
                    ),
                ));
            }
        } else if !f.not_null() && is_not_null {
            plan.ddl(format!("ALTER TABLE {} ALTER COLUMN {col} DROP NOT NULL", table.sql()));
        }
        Ok(())
    }

    async fn class(&self, conn: &mut PgConnection, plan: &mut Plan, class_id: Uuid) -> Result<(), AppError> {
        let Some(table) = self.model.table(class_id) else { return Ok(()) };
        if !self.catalog.has_table(&table) {
            if self.catalog.relation_exists(table.schema.as_str(), table.table.as_str()) {
                return Err(invalid_name(
                    "key",
                    "name_taken",
                    format!("{} already exists in the database and is not a ShadouCMDB table", table.display()),
                ));
            }
            let i = plan.ddl(format!(
                "CREATE TABLE {} (id uuid PRIMARY KEY REFERENCES cmdb.configuration_items (id) ON DELETE CASCADE)",
                table.sql()
            ));
            plan.note(Some(i), "create_table", Some(0), format!("New table {}", table.display()));
        }
        for f in self.model.own_fields(class_id) {
            self.field(conn, plan, &table, f).await?;
        }
        Ok(())
    }
}

/// A CI is active while `valid_from <= now() < valid_until` (open-ended without
/// `valid_until`). Derived, never stored; `ci` is the registry row.
pub const ACTIVE_SQL: &str = "(ci.valid_from <= now() AND (ci.valid_until IS NULL OR now() < ci.valid_until))";

/// The reporting view of a type: registry columns, then the fields of every
/// ancestor and its own. Lookup fields show the value's key.
fn view_sql(model: &Model, class_id: Uuid, view: &TableName) -> Option<String> {
    let table = model.table(class_id)?;
    let mut columns = vec![
        "ci.id".to_owned(),
        "ci.ident".into(),
        "ci.label".into(),
        "cls.key AS type".into(),
        "ci.valid_from".into(),
        "ci.valid_until".into(),
        format!("{ACTIVE_SQL} AS active"),
        "ci.version AS record_version".into(),
        "ci.created_at".into(),
        "ci.updated_at".into(),
        "ci.deleted_at".into(),
    ];
    let mut joins = Vec::new();
    for (i, c) in model.lineage(class_id).iter().enumerate() {
        let t = model.table(c.id)?;
        let alias = format!("t{i}");
        if c.id != class_id {
            joins.push(format!("JOIN {} {alias} ON {alias}.id = ci.id", t.sql()));
        }
        let alias = if c.id == class_id { "t".to_owned() } else { alias };
        for f in model.own_fields(c.id) {
            let col = f.column();
            // A field named like a registry column (possible before SHAA-56) gets a suffix in the view.
            let name = if naming::REGISTRY_VIEW_COLUMNS.contains(&f.key.as_str()) {
                quote_ident(&format!("{}_value", &f.key[..f.key.len().min(57)]))
            } else {
                col.to_string()
            };
            columns.push(match f.data_type {
                AttributeDataType::Lookup => {
                    format!("(SELECT lv.key FROM cmdb.lookup_list_values lv WHERE lv.id = {alias}.{col}) AS {name}")
                }
                _ => format!("{alias}.{col} AS {name}"),
            });
        }
    }
    Some(format!(
        "CREATE VIEW {} AS SELECT {} FROM {} t \
         JOIN cmdb.configuration_items ci ON ci.id = t.id \
         JOIN cmdb.ci_classes cls ON cls.id = ci.class_id{}{}",
        view.sql(),
        columns.join(", "),
        table.sql(),
        if joins.is_empty() { "" } else { " " },
        joins.join(" ")
    ))
}

fn view_comment(sql: &str) -> String {
    format!("shadoucmdb:{}", &hex::encode(Sha256::digest(sql.as_bytes()))[..16])
}

/// A view the engine created: its comment carries the `shadoucmdb:` marker.
/// Any other view of the same name belongs to someone else and is left alone.
fn is_engine_view(comment: Option<Option<&str>>) -> bool {
    comment.flatten().is_some_and(|c| c.starts_with("shadoucmdb:"))
}

async fn build(
    conn: &mut PgConnection,
    model: &Model,
    scope: &Scope,
    purge: &Purge,
    lenient_not_null: bool,
    visible: Option<&[Uuid]>,
) -> Result<Plan, AppError> {
    let mut schemas: BTreeSet<String> = model.areas.iter().map(|a| a.key.clone()).collect();
    schemas.extend(purge.schemas.iter().map(|s| s.as_str().to_owned()));
    let catalog = Catalog::load(conn, &schemas.into_iter().collect::<Vec<_>>()).await?;
    let planner = Planner { model, catalog: &catalog, lenient_not_null, visible };
    let mut plan = Plan::default();

    // Area schemas, and the reporting role's access to them.
    for area in &model.areas {
        let schema = Ident::trusted(&area.key);
        if !catalog.schemas.contains(&area.key) {
            let i = plan.ddl(format!("CREATE SCHEMA {schema}"));
            plan.note(Some(i), "create_schema", None, format!("New schema {}", area.key));
        }
        if let Some(r) = &catalog.reporting
            && !r.schemas.contains(&area.key)
        {
            plan.post.push(format!("GRANT USAGE ON SCHEMA {schema} TO {REPORTING_ROLE}"));
        }
    }

    let classes: Vec<Uuid> = match scope {
        Scope::All => model.classes.iter().map(|c| c.id).collect(),
        Scope::Classes(ids) => ids.iter().copied().filter(|id| model.class(*id).is_some()).collect(),
        Scope::Areas => Vec::new(),
    };
    for id in &classes {
        planner.class(conn, &mut plan, *id).await?;
    }

    // Purges: columns, then tables, then schemas.
    let counts = purge.reveals(visible);
    for (table, column) in &purge.columns {
        if catalog.column(table, column.as_str()).is_none() {
            continue;
        }
        let i = plan.ddl(format!("ALTER TABLE {} DROP COLUMN {column}", table.sql()));
        let without = format!("The stored values of {}.{column} are deleted", table.display());
        if counts {
            let n = count(conn, format!("SELECT count(*) FROM {} WHERE {column} IS NOT NULL", table.sql())).await?;
            let message = format!("{n} stored values of {}.{column} are deleted", table.display());
            plan.counted(Some(i), "drop_column", n, message, without, &purge.classes);
        } else {
            plan.note(Some(i), "drop_column", None, without);
        }
        plan.rebuild.insert(table.clone());
    }
    for table in &purge.tables {
        let view_name = format!("v_{}", table.table.as_str());
        if naming::is_identifier(&view_name) {
            let view = TableName { schema: table.schema.clone(), table: Ident::trusted(&view_name) };
            let existing = catalog.view_comment(&view);
            if is_engine_view(existing) {
                plan.pre.push(format!("DROP VIEW {}", view.sql()));
            } else if existing.is_some() {
                plan.note(None, "warning", None, format!("{} is not a ShadouCMDB view; left in place", view.display()));
            }
        }
        if !catalog.has_table(table) {
            continue;
        }
        let i = plan.ddl(format!("DROP TABLE {}", table.sql()));
        let without = format!("Table {} and its rows are deleted", table.display());
        if counts {
            let n = match purge.rows.get(table) {
                Some(n) => *n,
                None => count(conn, format!("SELECT count(*) FROM {}", table.sql())).await?,
            };
            let message = format!("Table {} and its {n} rows are deleted", table.display());
            plan.counted(Some(i), "drop_table", n, message, without, &purge.classes);
        } else {
            plan.note(Some(i), "drop_table", None, without);
        }
        plan.rebuild.insert(table.clone());
    }
    for schema in &purge.schemas {
        if catalog.schemas.contains(schema.as_str()) {
            let i = plan.ddl(format!("DROP SCHEMA {schema}"));
            plan.note(Some(i), "drop_schema", None, format!("Schema {} is dropped", schema.as_str()));
        }
    }

    // Reporting views: rebuilt when their definition changed or a table they read was altered.
    // A restore at a migration level before 0016 has the old registry columns; the
    // views follow once the newer migrations have run.
    let core_registry: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid = 'cmdb.configuration_items'::regclass
                          AND attname = 'label' AND NOT attisdropped)",
    )
    .fetch_one(&mut *conn)
    .await?;
    if !core_registry {
        return Ok(plan);
    }
    let mut affected: BTreeSet<Uuid> = BTreeSet::new();
    match scope {
        Scope::All => affected.extend(model.classes.iter().map(|c| c.id)),
        _ => {
            for id in &classes {
                affected.extend(model.subtree(*id));
            }
        }
    }
    for c in &model.classes {
        if model.lineage(c.id).iter().any(|l| model.table(l.id).is_some_and(|t| plan.rebuild.contains(&t))) {
            affected.insert(c.id);
        }
    }
    for id in affected {
        let Some(view) = model.view(id) else {
            if let Some(t) = model.table(id) {
                plan.note(
                    None,
                    "warning",
                    None,
                    format!("{} has no reporting view: its name is too long", t.display()),
                );
            }
            continue;
        };
        let Some(sql) = view_sql(model, id, &view) else { continue };
        let comment = view_comment(&sql);
        let existing = catalog.view_comment(&view);
        let lineage_altered =
            model.lineage(id).iter().any(|l| model.table(l.id).is_some_and(|t| plan.rebuild.contains(&t)));
        let current = existing == Some(Some(comment.as_str()));
        if !current || lineage_altered {
            if is_engine_view(existing) {
                plan.pre.push(format!("DROP VIEW {}", view.sql()));
            } else if catalog.relation_exists(view.schema.as_str(), view.table.as_str()) {
                plan.note(
                    None,
                    "warning",
                    None,
                    format!("{} exists and is not a ShadouCMDB view; skipped", view.display()),
                );
                continue;
            }
            plan.post.push(sql);
            plan.post.push(format!("COMMENT ON VIEW {} IS '{comment}'", view.sql()));
            if catalog.reporting.is_some() {
                plan.post.push(format!("GRANT SELECT ON {} TO {REPORTING_ROLE}", view.sql()));
            }
        } else if let Some(r) = &catalog.reporting
            && !r.views.contains(&(view.schema.as_str().to_owned(), view.table.as_str().to_owned()))
        {
            plan.post.push(format!("GRANT SELECT ON {} TO {REPORTING_ROLE}", view.sql()));
        }
    }
    Ok(plan)
}

// ---------------------------------------------------------------------------
// Apply
// ---------------------------------------------------------------------------

tokio::task_local! {
    static PREVIEW: RefCell<Vec<SchemaChange>>;
}

/// Runs `f` and returns what it returned plus every schema change it applied
/// (the caller rolls the transaction back for a preview).
pub async fn collect_previews<T>(f: impl Future<Output = T>) -> (T, Vec<SchemaChange>) {
    PREVIEW
        .scope(RefCell::new(Vec::new()), async move {
            let out = f.await;
            let changes = PREVIEW.with(|p| p.take());
            (out, changes)
        })
        .await
}

async fn execute(conn: &mut PgConnection, sql: &str) -> Result<(), AppError> {
    match sqlx::raw_sql(AssertSqlSafe(sql.to_owned())).execute(&mut *conn).await {
        Ok(_) => Ok(()),
        Err(err) => {
            let pg = err.as_database_error().map(|e| (e.code().map(|c| c.to_string()), e.message().to_owned()));
            match pg {
                // lock_not_available: a long query holds the table.
                Some((Some(code), msg)) if code == "55P03" => Err(AppError::conflict(format!(
                    "The table is busy (another session holds a lock on it); try again in a moment. ({msg})"
                ))),
                // Dependent objects a DBA created (views, foreign keys) block a drop.
                Some((Some(code), msg)) if code == "2BP01" => Err(AppError::new(
                    ErrorCode::InUse,
                    format!("Other database objects depend on it: {msg}. Remove them first."),
                )),
                // A foreign key or check that existing rows violate.
                Some((Some(code), msg)) if code.starts_with("23") => {
                    Err(AppError::new(ErrorCode::Conflict, format!("Existing data does not allow this change: {msg}")))
                }
                _ => Err(err.into()),
            }
        }
    }
}

/// Brings the physical schema in line with the metadata for `scope`, drops
/// what `purge` names, and records what ran. Returns None when nothing had to
/// change. Runs in the caller's transaction.
pub async fn apply(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    summary: impl Into<Summary>,
    scope: Scope,
    purge: Purge,
) -> Result<Option<SchemaChange>, AppError> {
    apply_with(conn, ctx, summary, scope, purge, false).await
}

/// The record keeps, next to what the caller sees, the types its counts
/// describe and a variant without them: GET /schema-changes shows that variant
/// to a reader who may not view every type counted (GH#252).
pub async fn apply_with(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    summary: impl Into<Summary>,
    scope: Scope,
    purge: Purge,
    lenient_not_null: bool,
) -> Result<Option<SchemaChange>, AppError> {
    let summary = summary.into();
    lock(conn).await?;
    let model = Model::load(conn).await?;
    let visible = visible_classes(conn, ctx).await?;
    let plan = build(conn, &model, &scope, &purge, lenient_not_null, visible.as_deref()).await?;
    if plan.is_empty() {
        return Ok(None);
    }
    // Wait at most this long for a table lock rather than queueing behind a long report.
    sqlx::query("SET LOCAL lock_timeout = '10s'").execute(&mut *conn).await?;
    let statements = plan.statements();
    for sql in &statements {
        execute(conn, sql).await?;
    }
    sqlx::query("SET LOCAL lock_timeout = DEFAULT").execute(&mut *conn).await?;

    let mut counted = plan.counted.clone();
    if summary.without_counts.is_some() {
        counted.extend(&purge.classes);
    }
    let redacted_impact = plan.redacted_impacts();
    let redacted_summary = match (&summary.without_counts, &redacted_impact) {
        (Some(s), _) => Some(s.clone()),
        (None, Some(_)) => Some(summary.text.clone()),
        (None, None) => None,
    };
    let redacted_impact =
        redacted_summary.as_ref().map(|_| sqlx::types::Json(redacted_impact.unwrap_or_else(|| plan.impacts())));
    let change: SchemaChange = sqlx::query_as(
        "INSERT INTO cmdb.schema_changes (actor_type, actor_id, actor_name, request_id, summary, statements, impact,
                                          count_classes, redacted_summary, redacted_impact)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING id, occurred_at, actor_type, actor_id, actor_name, request_id, summary, statements, impact",
    )
    .bind(ctx.actor.actor_type.as_str())
    .bind(&ctx.actor.id)
    .bind(&ctx.actor.name)
    .bind(&ctx.request_id)
    .bind(&summary.text)
    .bind(&statements)
    .bind(sqlx::types::Json(plan.impacts()))
    .bind(counted.into_iter().collect::<Vec<_>>())
    .bind(redacted_summary)
    .bind(redacted_impact)
    .fetch_one(&mut *conn)
    .await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: "schema_changes",
        entity_id: change.id,
        old_value: None,
        new_value: Some(crud::json(&change)),
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    let _ = PREVIEW.try_with(|p| p.borrow_mut().push(change.clone()));
    Ok(Some(change))
}

/// Full reconcile: every area, type, field and view (after `migrate`, after an
/// import, or when an administrator asks). A required field that still has
/// assets without a value stays nullable and is reported as a warning.
pub async fn reconcile(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    summary: &str,
) -> Result<Option<SchemaChange>, AppError> {
    apply_with(conn, ctx, summary, Scope::All, Purge::default(), true).await
}

/// Builds the physical schema from the metadata (a full, lenient reconcile)
/// without recording it in cmdb.schema_changes or the audit log. For
/// `shadoucmdb restore`: the restored history already describes the objects
/// it recreates. Returns the statements run and the warnings.
pub async fn rebuild_unrecorded(conn: &mut PgConnection) -> Result<(Vec<String>, Vec<String>), AppError> {
    lock(conn).await?;
    let model = Model::load(conn).await?;
    let plan = build(conn, &model, &Scope::All, &Purge::default(), true, None).await?;
    let statements = plan.statements();
    for sql in &statements {
        execute(conn, sql).await?;
    }
    let warnings = plan.impacts().into_iter().filter(|i| i.kind == "warning").map(|i| i.message).collect();
    Ok((statements, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_hash_matches_the_migration() {
        // SQL: left(encode(sha256(convert_to(string_agg(v, E'\n' ORDER BY n), 'UTF8')), 'hex'), 12)
        let v: Vec<String> = ["switch", "router"].iter().map(|s| s.to_string()).collect();
        assert_eq!(enum_hash(&v), hex::encode(Sha256::digest(b"switch\nrouter"))[..12]);
        assert_eq!(enum_hash(&v).len(), 12);
    }

    #[test]
    fn text_expressions() {
        let c = Ident::trusted("mgmt_ip");
        assert_eq!(text_expr(&c, "inet"), "abbrev(\"mgmt_ip\")");
        assert_eq!(text_expr(&c, "numeric"), "\"mgmt_ip\"::text");
    }

    #[test]
    fn only_marked_views_belong_to_the_engine() {
        assert!(is_engine_view(Some(Some("shadoucmdb:0123456789abcdef"))));
        assert!(!is_engine_view(Some(Some("reporting view for finance"))));
        assert!(!is_engine_view(Some(None)));
        assert!(!is_engine_view(None));
    }

    async fn sql(conn: &mut PgConnection, sql: &str) {
        sqlx::raw_sql(AssertSqlSafe(sql.to_owned())).execute(&mut *conn).await.unwrap();
    }

    async fn view_column(conn: &mut PgConnection, view: &TableName) -> Option<String> {
        sqlx::query_scalar(
            "SELECT attname::text FROM pg_attribute
             WHERE attrelid = to_regclass($1) AND attnum = 1 AND NOT attisdropped",
        )
        .bind(view.sql())
        .fetch_optional(&mut *conn)
        .await
        .unwrap()
    }

    fn warned(change: &Option<SchemaChange>, view: &TableName) -> bool {
        change
            .as_ref()
            .is_some_and(|c| c.impact.0.iter().any(|i| i.kind == "warning" && i.message.contains(&view.display())))
    }

    /// GH#61: a view named `<area>.v_<type>` that the engine did not create
    /// (no comment, or a comment without the marker) survives a reconcile.
    #[tokio::test]
    async fn a_reconcile_leaves_foreign_views_alone() {
        let Some(db) = crate::db::scratch::database("schema_foreign_views").await else { return };
        crate::seed::install_template(&db.pool, "it_infrastructure").await.unwrap();
        let mut c = db.pool.acquire().await.unwrap();
        let model = Model::load(&mut c).await.unwrap();
        let views: Vec<TableName> = model.classes.iter().filter_map(|k| model.view(k.id)).take(3).collect();
        let [bare, commented, missing] = views.as_slice() else { panic!("the template has at least three types") };
        for v in [bare, commented] {
            assert_eq!(view_column(&mut c, v).await.as_deref(), Some("id"), "{} is the engine's view", v.display());
            sql(&mut c, &format!("DROP VIEW {}; CREATE VIEW {} AS SELECT 1 AS x", v.sql(), v.sql())).await;
        }
        sql(&mut c, &format!("COMMENT ON VIEW {} IS 'finance report'", commented.sql())).await;
        // Something to rebuild, so that the reconcile records a change (and its warnings).
        sql(&mut c, &format!("DROP VIEW {}", missing.sql())).await;

        let ctx = RequestContext::system("test", "test");
        let mut tx = db.pool.begin().await.unwrap();
        let change = reconcile(&mut tx, &ctx, "reconcile").await.unwrap();
        tx.commit().await.unwrap();
        for v in [bare, commented] {
            assert_eq!(view_column(&mut c, v).await.as_deref(), Some("x"), "{} was replaced", v.display());
            assert!(warned(&change, v), "no warning for {}", v.display());
        }
        assert_eq!(view_column(&mut c, missing).await.as_deref(), Some("id"), "the engine's own view is rebuilt");
        drop(c);
        db.drop().await;
    }

    /// GH#61: purging a table drops the engine's view of it, never a foreign one.
    #[tokio::test]
    async fn a_purge_leaves_foreign_views_alone() {
        let Some(db) = crate::db::scratch::database("schema_purge_foreign_views").await else { return };
        crate::seed::install_template(&db.pool, "it_infrastructure").await.unwrap();
        let mut c = db.pool.acquire().await.unwrap();
        let area = Model::load(&mut c).await.unwrap().areas[0].key.clone();
        let name = |t: &str| TableName { schema: Ident::trusted(&area), table: Ident::trusted(t) };
        let (ours, theirs) = (name("gh61_ours"), name("gh61_theirs"));
        let (v_ours, v_theirs) = (name("v_gh61_ours"), name("v_gh61_theirs"));
        sql(
            &mut c,
            &format!(
                "CREATE TABLE {} (id int); CREATE TABLE {} (id int);
                 CREATE VIEW {v_o} AS SELECT 1 AS x; COMMENT ON VIEW {v_o} IS 'shadoucmdb:0123456789abcdef';
                 CREATE VIEW {v_t} AS SELECT 1 AS x; COMMENT ON VIEW {v_t} IS 'finance report';",
                ours.sql(),
                theirs.sql(),
                v_o = v_ours.sql(),
                v_t = v_theirs.sql(),
            ),
        )
        .await;
        let purge = Purge { tables: vec![ours.clone(), theirs.clone()], ..Purge::default() };

        let ctx = RequestContext::system("test", "test");
        let mut tx = db.pool.begin().await.unwrap();
        let change = apply(&mut tx, &ctx, "purge", Scope::Areas, purge).await.unwrap();
        tx.commit().await.unwrap();
        let statements = &change.as_ref().unwrap().statements;
        assert!(statements.contains(&format!("DROP VIEW {}", v_ours.sql())), "{statements:?}");
        assert!(!statements.iter().any(|s| s.contains("v_gh61_theirs")), "{statements:?}");
        assert!(warned(&change, &v_theirs));
        assert_eq!(view_column(&mut c, &v_ours).await, None);
        assert_eq!(view_column(&mut c, &v_theirs).await.as_deref(), Some("x"));
        assert_eq!(view_column(&mut c, &theirs).await, None, "the purged table is gone");
        drop(c);
        db.drop().await;
    }

    /// GH#60: datetime -> date is refused while a value has a time of day, and
    /// converts by the UTC day (not the session TimeZone) once none has.
    #[tokio::test]
    async fn datetime_to_date_is_refused_unless_lossless() {
        const TEST: &str = "datetime_to_date_is_refused_unless_lossless";
        let Some(db) = crate::db::scratch::database(TEST).await else { return };
        let mut c = db.pool.acquire().await.unwrap();
        let conn: &mut PgConnection = &mut c;
        // Far from UTC: a conversion through text would move 2024-05-01 00:00Z to 2024-04-30.
        sqlx::raw_sql("SET TimeZone = 'America/Los_Angeles'; CREATE TABLE public.t (last_seen timestamptz)")
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::raw_sql("INSERT INTO public.t VALUES ('2024-05-01T23:30:00Z'), ('2024-05-02T00:00:00Z'), (NULL)")
            .execute(&mut *conn)
            .await
            .unwrap();
        let table = TableName { schema: Ident::trusted("public"), table: Ident::trusted("t") };
        let mut f = Field {
            id: Uuid::new_v4(),
            class_id: Uuid::new_v4(),
            key: "last_seen".into(),
            label: "Last seen".into(),
            data_type: AttributeDataType::Date,
            enum_values: None,
            is_required: false,
            is_expected: false,
            is_active: true,
            sort_order: 0,
            lookup_list_id: None,
            system_role: None,
        };
        let run = async |conn: &mut PgConnection, f: &Field, from: &str| {
            let mut plan = Plan::default();
            type_change(conn, &mut plan, &table, f, from, &[]).await?;
            for sql in &plan.ddl {
                execute(conn, sql).await?;
            }
            Ok::<_, AppError>(())
        };

        let err = run(&mut *conn, &f, "timestamp with time zone").await.unwrap_err();
        let detail = &err.details.as_ref().unwrap()[0];
        assert_eq!(detail.code, "type_change_lossy");
        assert!(err.message.contains("1 of 2") && err.message.contains("\"2024-05-01T23:30:00Z\""), "{}", err.message);

        sqlx::raw_sql(
            "UPDATE public.t SET last_seen = '2024-05-01T00:00:00Z' WHERE last_seen = '2024-05-01T23:30:00Z'",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
        run(&mut *conn, &f, "timestamp with time zone").await.unwrap();
        let days: Vec<String> =
            sqlx::query_scalar("SELECT last_seen::text FROM public.t WHERE last_seen IS NOT NULL ORDER BY 1")
                .fetch_all(&mut *conn)
                .await
                .unwrap();
        assert_eq!(days, ["2024-05-01", "2024-05-02"]);

        // And back: a date is midnight UTC, whatever the session TimeZone.
        f.data_type = AttributeDataType::Datetime;
        run(&mut *conn, &f, "date").await.unwrap();
        let back: Vec<String> = sqlx::query_scalar(
            "SELECT to_char(last_seen AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI') FROM public.t WHERE last_seen IS NOT NULL ORDER BY 1",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(back, ["2024-05-01 00:00", "2024-05-02 00:00"]);

        drop(c);
        db.drop().await;
    }
}
