//! The data model as the metadata tables describe it: areas, types and their
//! fields, with the physical names each maps to. Loaded per request (three
//! small queries); the DDL engine diffs it against the catalog, the CI service
//! uses it to read and write the type tables.

use sqlx::PgConnection;
use sqlx::types::Json;
use uuid::Uuid;

use super::naming::{Ident, is_identifier};
use crate::modules::classes::AttributeDataType;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Area {
    pub id: Uuid,
    pub key: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Class {
    pub id: Uuid,
    pub key: String,
    pub area_id: Uuid,
    pub parent_id: Option<Uuid>,
    /// The field whose value labels the class's CIs (in its lineage)
    pub title_attribute_id: Option<Uuid>,
    /// The field holding a CI's owner (data quality); `None` takes the parent's
    pub owner_attribute_id: Option<Uuid>,
    /// The date or datetime field holding a CI's end of life; `None` takes the parent's
    pub end_of_life_attribute_id: Option<Uuid>,
}

/// A per-type setting that names a field for the data-quality checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityField {
    Owner,
    EndOfLife,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Field {
    pub id: Uuid,
    pub class_id: Uuid,
    pub key: String,
    pub label: String,
    pub data_type: AttributeDataType,
    pub enum_values: Option<Json<Vec<String>>>,
    pub is_required: bool,
    /// Counts towards completeness without being enforced (migration 0067)
    pub is_expected: bool,
    pub is_active: bool,
    pub sort_order: i32,
    pub lookup_list_id: Option<Uuid>,
    /// `person_name` or `person_email` for the key fields of the built-in
    /// Person type (migration 0044); the Person's email is unique ignoring case and Unicode form.
    pub system_role: Option<String>,
}

/// PostgreSQL column type of a field's data type.
pub fn pg_type(t: AttributeDataType) -> &'static str {
    use AttributeDataType as T;
    match t {
        T::Text | T::Enum => "text",
        T::Number => "numeric",
        T::Integer => "bigint",
        T::Boolean => "boolean",
        T::Date => "date",
        T::Datetime => "timestamp with time zone",
        T::Ip => "inet",
        T::Cidr => "cidr",
        T::Reference | T::Lookup => "uuid",
    }
}

impl Field {
    pub fn enum_list(&self) -> &[String] {
        self.enum_values.as_ref().map(|v| v.0.as_slice()).unwrap_or_default()
    }

    /// A required, active field is NOT NULL in its table.
    pub fn not_null(&self) -> bool {
        self.is_required && self.is_active
    }

    /// An active field a complete CI holds a value for: required or expected.
    pub fn counts_for_completeness(&self) -> bool {
        self.is_active && (self.is_required || self.is_expected)
    }

    pub fn column(&self) -> Ident {
        Ident::trusted(&self.key)
    }

    /// The Person's Email: unique across Person CIs, ignoring case.
    pub fn is_unique_email(&self) -> bool {
        self.system_role.as_deref() == Some("person_email")
    }

    /// Hex of the id: the stable part of the field's constraint and index names.
    pub fn hex(&self) -> String {
        self.id.simple().to_string()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Model {
    pub areas: Vec<Area>,
    pub classes: Vec<Class>,
    pub fields: Vec<Field>,
}

/// Where a type's rows live.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TableName {
    pub schema: Ident,
    pub table: Ident,
}

impl TableName {
    /// `"bestand"."netzwerk"`
    pub fn sql(&self) -> String {
        format!("{}.{}", self.schema, self.table)
    }

    /// `bestand.netzwerk` (for people: every technical name is a plain identifier)
    pub fn display(&self) -> String {
        format!("{}.{}", self.schema.as_str(), self.table.as_str())
    }
}

impl Model {
    pub async fn load(conn: &mut PgConnection) -> sqlx::Result<Model> {
        let areas = Self::load_areas(conn).await?;
        let classes = Self::load_classes(conn).await?;
        let fields = Self::load_fields(conn).await?;
        Ok(Model { areas, classes, fields })
    }

    // The statements of `load` one by one, for a caller that limits each
    // statement's time (impact analysis, GH#393).

    pub async fn load_areas(conn: &mut PgConnection) -> sqlx::Result<Vec<Area>> {
        sqlx::query_as::<_, Area>("SELECT id, key FROM cmdb.areas ORDER BY sort_order, key").fetch_all(&mut *conn).await
    }

    pub async fn load_classes(conn: &mut PgConnection) -> sqlx::Result<Vec<Class>> {
        // Through to_jsonb: a restore builds type tables at older migration levels (before 0016).
        sqlx::query_as::<_, Class>(
            "SELECT id, key, area_id, parent_id, (to_jsonb(c) ->> 'title_attribute_id')::uuid AS title_attribute_id,
                    (to_jsonb(c) ->> 'owner_attribute_id')::uuid AS owner_attribute_id,
                    (to_jsonb(c) ->> 'end_of_life_attribute_id')::uuid AS end_of_life_attribute_id
             FROM cmdb.ci_classes c ORDER BY key",
        )
        .fetch_all(&mut *conn)
        .await
    }

    pub async fn load_fields(conn: &mut PgConnection) -> sqlx::Result<Vec<Field>> {
        sqlx::query_as::<_, Field>(
            "SELECT id, class_id, key, label, data_type, enum_values, is_required,
                    coalesce((to_jsonb(d) ->> 'is_expected')::boolean, false) AS is_expected, is_active, sort_order,
                    lookup_list_id, to_jsonb(d) ->> 'system_role' AS system_role
             FROM cmdb.ci_attribute_definitions d ORDER BY sort_order, key",
        )
        .fetch_all(&mut *conn)
        .await
    }

    pub fn area(&self, id: Uuid) -> Option<&Area> {
        self.areas.iter().find(|a| a.id == id)
    }

    pub fn class(&self, id: Uuid) -> Option<&Class> {
        self.classes.iter().find(|c| c.id == id)
    }

    /// The table of a type (None if its metadata is inconsistent, which the constraints prevent).
    pub fn table(&self, class_id: Uuid) -> Option<TableName> {
        let class = self.class(class_id)?;
        let area = self.area(class.area_id)?;
        (is_identifier(&area.key) && is_identifier(&class.key))
            .then(|| TableName { schema: Ident::trusted(&area.key), table: Ident::trusted(&class.key) })
    }

    /// The reporting view of a type: `<area>.v_<type>` (None for a key too long for the prefix).
    pub fn view(&self, class_id: Uuid) -> Option<TableName> {
        let t = self.table(class_id)?;
        let name = format!("v_{}", t.table.as_str());
        is_identifier(&name).then(|| TableName { schema: t.schema, table: Ident::trusted(&name) })
    }

    pub fn field(&self, id: Uuid) -> Option<&Field> {
        self.fields.iter().find(|f| f.id == id)
    }

    /// The field that labels CIs of this type (None: they are labelled by their ident).
    pub fn title_field(&self, class_id: Uuid) -> Option<&Field> {
        self.class(class_id)?.title_attribute_id.and_then(|id| self.field(id))
    }

    /// The owner or end-of-life field of a type: its own setting, else the
    /// nearest ancestor's. A setting naming a field outside the lineage (left
    /// behind by a move) is skipped.
    pub fn quality_field(&self, class_id: Uuid, which: QualityField) -> Option<&Field> {
        let lineage = self.lineage(class_id);
        lineage.iter().rev().find_map(|c| {
            let id = match which {
                QualityField::Owner => c.owner_attribute_id,
                QualityField::EndOfLife => c.end_of_life_attribute_id,
            }?;
            self.field(id).filter(|f| lineage.iter().any(|l| l.id == f.class_id))
        })
    }

    /// Where values of this lookup list are stored: (table, column) of every lookup field using it.
    pub fn lookup_columns(&self, list_id: Uuid) -> Vec<(TableName, Ident)> {
        self.fields
            .iter()
            .filter(|f| f.data_type == AttributeDataType::Lookup && f.lookup_list_id == Some(list_id))
            .filter_map(|f| Some((self.table(f.class_id)?, f.column())))
            .collect()
    }

    /// Fields defined directly on a type, in form order.
    pub fn own_fields(&self, class_id: Uuid) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(move |f| f.class_id == class_id)
    }

    /// The type and its ancestors, root first.
    pub fn lineage(&self, class_id: Uuid) -> Vec<&Class> {
        let mut out = Vec::new();
        let mut next = self.class(class_id);
        while let Some(c) = next {
            if out.len() > 64 || out.iter().any(|x: &&Class| x.id == c.id) {
                break;
            }
            out.push(c);
            next = c.parent_id.and_then(|p| self.class(p));
        }
        out.reverse();
        out
    }

    /// The type and every type below it.
    pub fn subtree(&self, class_id: Uuid) -> Vec<Uuid> {
        let mut out = vec![class_id];
        let mut i = 0;
        while i < out.len() {
            let parent = out[i];
            for c in self.classes.iter().filter(|c| c.parent_id == Some(parent)) {
                if !out.contains(&c.id) {
                    out.push(c.id);
                }
            }
            i += 1;
        }
        out
    }
}
