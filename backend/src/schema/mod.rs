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
use std::collections::{BTreeSet, HashSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{AssertSqlSafe, PgConnection};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::context::RequestContext;
use crate::api::schemas::ts;
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
    /// Rows (assets) concerned, when known
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
}

#[derive(Default)]
struct Plan {
    /// Views dropped before the table changes that they depend on.
    pre: Vec<String>,
    ddl: Vec<String>,
    /// Views (re)created and grants, after the table changes.
    post: Vec<String>,
    /// (index into ddl, impact)
    impact: Vec<(Option<usize>, Impact)>,
    /// Tables whose dependent views must be rebuilt (a column type changed or was dropped).
    rebuild: HashSet<TableName>,
}

impl Plan {
    fn ddl(&mut self, sql: String) -> usize {
        self.ddl.push(sql);
        self.ddl.len() - 1
    }

    fn note(&mut self, statement: Option<usize>, kind: &str, rows: Option<i64>, message: String) {
        self.impact.push((statement, Impact { statement: None, kind: kind.into(), rows, message }));
    }

    fn is_empty(&self) -> bool {
        self.pre.is_empty() && self.ddl.is_empty() && self.post.is_empty()
    }

    fn statements(&self) -> Vec<String> {
        self.pre.iter().chain(&self.ddl).chain(&self.post).cloned().collect()
    }

    fn impacts(&self) -> Vec<Impact> {
        let offset = self.pre.len();
        self.impact.iter().map(|(i, imp)| Impact { statement: i.map(|i| i + offset), ..imp.clone() }).collect()
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

/// Stored values (as text) that are not in the allowed list.
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

/// `ALTER COLUMN .. TYPE .. USING ..` for a type change, after a dry run of the
/// conversion over every stored value.
async fn type_change(
    conn: &mut PgConnection,
    plan: &mut Plan,
    table: &TableName,
    f: &Field,
    from: &str,
) -> Result<(), AppError> {
    let col = f.column();
    let to = pg_type(f.data_type);
    let rows = count(conn, format!("SELECT count(*) FROM {} WHERE {col} IS NOT NULL", table.sql())).await?;
    let (using, bad): (String, Option<String>) = match (from, to) {
        (_, "text") => (text_expr(&col, from), None),
        ("bigint", "numeric") => (format!("{col}::numeric"), None),
        ("numeric", "bigint") => (
            format!("{col}::bigint"),
            Some(format!("{col} <> trunc({col}) OR {col} NOT BETWEEN -9223372036854775808 AND 9223372036854775807")),
        ),
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
            (format!("({text})::{to}"), Some(format!("NOT cmdb.value_castable({text}, '{to}'::regtype)")))
        }
    };
    if let Some(bad) = bad {
        let where_bad = format!("FROM {} WHERE {col} IS NOT NULL AND ({bad})", table.sql());
        let failing = count(conn, format!("SELECT count(*) {where_bad}")).await?;
        if failing > 0 {
            let sample =
                samples(conn, format!("SELECT DISTINCT {} {where_bad} ORDER BY 1 LIMIT 5", text_expr(&col, from)))
                    .await?;
            return Err(field_error(
                "dataType",
                "type_change_failed",
                format!(
                    "{failing} of {rows} stored values of \"{}\" cannot be converted to {}: {}. Correct or clear them \
                     first; nothing was changed.",
                    f.key,
                    f.data_type.as_str(),
                    quoted_list(&sample)
                ),
            ));
        }
    }
    let i = plan.ddl(format!("ALTER TABLE {} ALTER COLUMN {col} TYPE {to} USING {using}", table.sql()));
    plan.note(
        Some(i),
        "rewrite",
        Some(rows),
        format!(
            "{} values of {}.{} converted from {from} to {to} (dry run: all convert)",
            rows,
            table.display(),
            f.key
        ),
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
}

impl Planner<'_> {
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
            type_change(conn, plan, table, f, &current_type).await?;
        }

        // Enum: the CHECK carries a hash of the allowed values, so a changed list gets a new constraint.
        if let Some(name) = &wanted_check
            && !checks.iter().any(|c| &c.name == name)
        {
            if !is_new {
                let expr = text_expr(&col, &current_type);
                let (n, sample) = values_outside(conn, table, &expr, &col, f.enum_list()).await?;
                if n > 0 {
                    // No check yet: the field is becoming an enum; otherwise its list changed.
                    let field = if checks.is_empty() { "dataType" } else { "enumValues" };
                    return Err(field_error(
                        field,
                        "enum_value_in_use",
                        format!(
                            "{n} assets store values of \"{}\" that are not in the list: {}. Add them to the list or \
                             change those assets first.",
                            f.key,
                            quoted_list(&sample)
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

        // NOT NULL for a required, active field; refused while any asset has no value.
        let is_not_null = existing.is_some_and(|c| c.not_null);
        if f.not_null() && !is_not_null {
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
                plan.note(
                    None,
                    "warning",
                    Some(nulls),
                    format!("{}.{} stays nullable: {nulls} assets have no value", table.display(), f.key),
                );
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

/// The reporting view of a type: registry columns, then the fields of every
/// ancestor and its own. Lookup fields show the value's key.
fn view_sql(model: &Model, class_id: Uuid, view: &TableName) -> Option<String> {
    let table = model.table(class_id)?;
    let mut columns = vec![
        "ci.id".to_owned(),
        "ci.name".into(),
        "cls.key AS type".into(),
        "st.key AS status".into(),
        "env.key AS environment".into(),
        "own.name AS owner".into(),
        "loc.key AS location".into(),
        "ci.hostname".into(),
        "ci.ip_address".into(),
        "ci.serial_number".into(),
        "ci.notes".into(),
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
         JOIN cmdb.ci_classes cls ON cls.id = ci.class_id \
         JOIN cmdb.statuses st ON st.id = ci.status_id \
         LEFT JOIN cmdb.environments env ON env.id = ci.environment_id \
         LEFT JOIN cmdb.owners own ON own.id = ci.owner_id \
         LEFT JOIN cmdb.locations loc ON loc.id = ci.location_id{}{}",
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

async fn build(
    conn: &mut PgConnection,
    model: &Model,
    scope: &Scope,
    purge: &Purge,
    lenient_not_null: bool,
) -> Result<Plan, AppError> {
    let mut schemas: BTreeSet<String> = model.areas.iter().map(|a| a.key.clone()).collect();
    schemas.extend(purge.schemas.iter().map(|s| s.as_str().to_owned()));
    let catalog = Catalog::load(conn, &schemas.into_iter().collect::<Vec<_>>()).await?;
    let planner = Planner { model, catalog: &catalog, lenient_not_null };
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
    for (table, column) in &purge.columns {
        if catalog.column(table, column.as_str()).is_none() {
            continue;
        }
        let n = count(conn, format!("SELECT count(*) FROM {} WHERE {column} IS NOT NULL", table.sql())).await?;
        let i = plan.ddl(format!("ALTER TABLE {} DROP COLUMN {column}", table.sql()));
        plan.note(
            Some(i),
            "drop_column",
            Some(n),
            format!("{n} stored values of {}.{column} are deleted", table.display()),
        );
        plan.rebuild.insert(table.clone());
    }
    for table in &purge.tables {
        let view_name = format!("v_{}", table.table.as_str());
        if naming::is_identifier(&view_name) {
            let view = TableName { schema: table.schema.clone(), table: Ident::trusted(&view_name) };
            if catalog.view_comment(&view).is_some() {
                plan.pre.push(format!("DROP VIEW {}", view.sql()));
            }
        }
        if !catalog.has_table(table) {
            continue;
        }
        let n = count(conn, format!("SELECT count(*) FROM {}", table.sql())).await?;
        let i = plan.ddl(format!("DROP TABLE {}", table.sql()));
        plan.note(Some(i), "drop_table", Some(n), format!("Table {} and its {n} rows are deleted", table.display()));
        plan.rebuild.insert(table.clone());
    }
    for schema in &purge.schemas {
        if catalog.schemas.contains(schema.as_str()) {
            let i = plan.ddl(format!("DROP SCHEMA {schema}"));
            plan.note(Some(i), "drop_schema", None, format!("Schema {} is dropped", schema.as_str()));
        }
    }

    // Reporting views: rebuilt when their definition changed or a table they read was altered.
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
            if existing.is_some() {
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
    summary: &str,
    scope: Scope,
    purge: Purge,
) -> Result<Option<SchemaChange>, AppError> {
    apply_with(conn, ctx, summary, scope, purge, false).await
}

pub async fn apply_with(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    summary: &str,
    scope: Scope,
    purge: Purge,
    lenient_not_null: bool,
) -> Result<Option<SchemaChange>, AppError> {
    lock(conn).await?;
    let model = Model::load(conn).await?;
    let plan = build(conn, &model, &scope, &purge, lenient_not_null).await?;
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

    let change: SchemaChange = sqlx::query_as(
        "INSERT INTO cmdb.schema_changes (actor_type, actor_id, actor_name, request_id, summary, statements, impact)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id, occurred_at, actor_type, actor_id, actor_name, request_id, summary, statements, impact",
    )
    .bind(ctx.actor.actor_type.as_str())
    .bind(&ctx.actor.id)
    .bind(&ctx.actor.name)
    .bind(&ctx.request_id)
    .bind(summary)
    .bind(&statements)
    .bind(sqlx::types::Json(plan.impacts()))
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
    let plan = build(conn, &model, &Scope::All, &Purge::default(), true).await?;
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
}
