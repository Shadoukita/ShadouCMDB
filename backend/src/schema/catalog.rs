//! What exists in PostgreSQL right now, for the schemas the areas own.

use std::collections::{HashMap, HashSet};

use sqlx::PgConnection;

use super::model::TableName;

pub const REPORTING_ROLE: &str = "cmdb_reporting";

#[derive(Debug, Clone)]
pub struct Column {
    /// format_type(): "numeric", "timestamp with time zone", ...
    pub data_type: String,
    pub not_null: bool,
}

#[derive(Debug, Clone)]
pub struct Constraint {
    pub name: String,
    /// Foreign keys: "cmdb.configuration_items"
    pub references: Option<String>,
}

#[derive(Debug, Default)]
pub struct Catalog {
    pub schemas: HashSet<String>,
    /// (schema, table) -> columns by name
    pub tables: HashMap<(String, String), HashMap<String, Column>>,
    pub constraints: HashMap<(String, String), Vec<Constraint>>,
    /// (schema, index name)
    pub indexes: HashSet<(String, String)>,
    /// (schema, view) -> the view's comment (the engine's definition hash)
    pub views: HashMap<(String, String), Option<String>>,
    /// Some when the cmdb_reporting role exists
    pub reporting: Option<ReportingGrants>,
    /// Other relations in the area schemas (sequences, a DBA's tables, ...)
    pub other_relations: HashSet<(String, String)>,
}

#[derive(Debug, Default)]
pub struct ReportingGrants {
    pub schemas: HashSet<String>,
    pub views: HashSet<(String, String)>,
}

fn key(t: &TableName) -> (String, String) {
    (t.schema.as_str().to_owned(), t.table.as_str().to_owned())
}

impl Catalog {
    pub async fn load(conn: &mut PgConnection, schemas: &[String]) -> sqlx::Result<Catalog> {
        let mut cat = Catalog::default();
        let names: Vec<String> = schemas.to_vec();

        let found: Vec<String> = sqlx::query_scalar("SELECT nspname::text FROM pg_namespace WHERE nspname = ANY($1)")
            .bind(&names)
            .fetch_all(&mut *conn)
            .await?;
        cat.schemas = found.into_iter().collect();

        let relations: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT n.nspname::text, c.relname::text, c.relkind::text, obj_description(c.oid, 'pg_class')
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = ANY($1)",
        )
        .bind(&names)
        .fetch_all(&mut *conn)
        .await?;
        for (schema, name, kind, comment) in relations {
            match kind.as_str() {
                "r" | "p" => {
                    cat.tables.entry((schema, name)).or_default();
                }
                "v" => {
                    cat.views.insert((schema, name), comment);
                }
                "i" => {
                    cat.indexes.insert((schema, name));
                }
                _ => {
                    cat.other_relations.insert((schema, name));
                }
            }
        }

        let columns: Vec<(String, String, String, String, bool)> = sqlx::query_as(
            "SELECT n.nspname::text, c.relname::text, a.attname::text, format_type(a.atttypid, a.atttypmod), a.attnotnull
             FROM pg_attribute a
             JOIN pg_class c ON c.oid = a.attrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = ANY($1) AND c.relkind IN ('r', 'p') AND a.attnum > 0 AND NOT a.attisdropped",
        )
        .bind(&names)
        .fetch_all(&mut *conn)
        .await?;
        for (schema, table, column, data_type, not_null) in columns {
            cat.tables.entry((schema, table)).or_default().insert(column, Column { data_type, not_null });
        }

        let constraints: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT n.nspname::text, c.relname::text, con.conname::text,
                    CASE WHEN con.contype = 'f' THEN fn.nspname || '.' || fc.relname END
             FROM pg_constraint con
             JOIN pg_class c ON c.oid = con.conrelid
             JOIN pg_namespace n ON n.oid = c.relnamespace
             LEFT JOIN pg_class fc ON fc.oid = con.confrelid
             LEFT JOIN pg_namespace fn ON fn.oid = fc.relnamespace
             WHERE n.nspname = ANY($1)",
        )
        .bind(&names)
        .fetch_all(&mut *conn)
        .await?;
        for (schema, table, name, references) in constraints {
            cat.constraints.entry((schema, table)).or_default().push(Constraint { name, references });
        }

        let role_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1)")
            .bind(REPORTING_ROLE)
            .fetch_one(&mut *conn)
            .await?;
        if role_exists {
            let mut grants = ReportingGrants::default();
            let schemas: Vec<String> = sqlx::query_scalar(
                "SELECT nspname::text FROM pg_namespace WHERE nspname = ANY($1) AND has_schema_privilege($2, oid, 'USAGE')",
            )
            .bind(&names)
            .bind(REPORTING_ROLE)
            .fetch_all(&mut *conn)
            .await?;
            grants.schemas = schemas.into_iter().collect();
            let views: Vec<(String, String)> = sqlx::query_as(
                "SELECT n.nspname::text, c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname = ANY($1) AND c.relkind = 'v' AND has_table_privilege($2, c.oid, 'SELECT')",
            )
            .bind(&names)
            .bind(REPORTING_ROLE)
            .fetch_all(&mut *conn)
            .await?;
            grants.views = views.into_iter().collect();
            cat.reporting = Some(grants);
        }
        Ok(cat)
    }

    pub fn has_table(&self, t: &TableName) -> bool {
        self.tables.contains_key(&key(t))
    }

    pub fn column(&self, t: &TableName, column: &str) -> Option<&Column> {
        self.tables.get(&key(t)).and_then(|cols| cols.get(column))
    }

    pub fn constraints(&self, t: &TableName) -> &[Constraint] {
        self.constraints.get(&key(t)).map(Vec::as_slice).unwrap_or_default()
    }

    pub fn has_index(&self, schema: &str, name: &str) -> bool {
        self.indexes.contains(&(schema.to_owned(), name.to_owned()))
    }

    pub fn view_comment(&self, v: &TableName) -> Option<Option<&str>> {
        self.views.get(&key(v)).map(|c| c.as_deref())
    }

    /// A relation of any kind with this name exists in the schema.
    pub fn relation_exists(&self, schema: &str, name: &str) -> bool {
        let k = (schema.to_owned(), name.to_owned());
        self.tables.contains_key(&k)
            || self.views.contains_key(&k)
            || self.indexes.contains(&k)
            || self.other_relations.contains(&k)
    }
}
