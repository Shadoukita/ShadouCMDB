//! Typed, directional edges between CIs. Removal is a soft delete.

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::openapi::schema::Schema;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::simple_resource::non_empty;
use crate::api::context::RequestContext;
use crate::api::route::{Body, Check, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, route};
use crate::api::schemas::{self, Deleted, Page, Paged, Sort, UuidList, description_schema, like_pattern, ts, ts_opt};
use crate::auth::permissions::ClassOp;
use crate::data::crud::{self, AuditAction, AuditEntry, ColumnSet, Where};
use crate::data::relationships::{self as data, RelationshipRow};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::paged;

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipTypeRef {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub forward_label: String,
    pub reverse_label: String,
    pub is_directional: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Endpoint {
    pub id: Uuid,
    /// The CI's label
    pub name: String,
    pub class_key: String,
    pub class_name: String,
    pub deleted: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Relationship {
    pub id: Uuid,
    pub relationship_type_id: Uuid,
    #[schema(inline)]
    #[serde(rename = "type")]
    pub rel_type: RelationshipTypeRef,
    pub source_ci_id: Uuid,
    #[schema(inline)]
    pub source: Endpoint,
    pub target_ci_id: Uuid,
    #[schema(inline)]
    pub target: Endpoint,
    #[schema(required = true)]
    pub notes: Option<String>,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
    /// Set when the relationship was removed (soft delete)
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(required = true)]
    pub deleted_at: Option<DateTime<Utc>>,
}

impl From<RelationshipRow> for Relationship {
    fn from(r: RelationshipRow) -> Self {
        Relationship {
            id: r.id,
            relationship_type_id: r.relationship_type_id,
            rel_type: RelationshipTypeRef {
                id: r.relationship_type_id,
                key: r.type_key,
                name: r.type_name,
                forward_label: r.forward_label,
                reverse_label: r.reverse_label,
                is_directional: r.is_directional,
            },
            source_ci_id: r.source_ci_id,
            source: Endpoint {
                id: r.source_ci_id,
                name: r.source_name,
                class_key: r.source_class_key,
                class_name: r.source_class_name,
                deleted: r.source_deleted_at.is_some(),
            },
            target_ci_id: r.target_ci_id,
            target: Endpoint {
                id: r.target_ci_id,
                name: r.target_name,
                class_key: r.target_class_key,
                class_name: r.target_class_name,
                deleted: r.target_deleted_at.is_some(),
            },
            notes: r.notes,
            created_at: r.created_at,
            updated_at: r.updated_at,
            deleted_at: r.deleted_at,
        }
    }
}

fn sort_schema() -> Schema {
    schemas::sort_schema(data::SORT_FIELDS, "-createdAt")
}

fn ci_ids_schema() -> Schema {
    schemas::uuid_list_described("Relationships where any of these CIs is source or target")
}

fn deleted_schema() -> Schema {
    schemas::deleted_schema("Removed relationships: exclude (default), include, or only")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RelationshipList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = sort_schema)]
    sort: Sort,
    #[param(schema_with = ci_ids_schema)]
    ci_id: Option<UuidList>,
    #[param(schema_with = schemas::uuid_list_schema)]
    source_ci_id: Option<UuidList>,
    #[param(schema_with = schemas::uuid_list_schema)]
    target_ci_id: Option<UuidList>,
    #[param(schema_with = schemas::uuid_list_schema)]
    relationship_type_id: Option<UuidList>,
    #[param(required = false, schema_with = deleted_schema)]
    deleted: Deleted,
}
paged!(RelationshipList);

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipCreate {
    relationship_type_id: Uuid,
    /// Reads "source <forwardLabel> target", e.g. application runs on server
    source_ci_id: Uuid,
    target_ci_id: Uuid,
    #[schema(schema_with = description_schema)]
    #[serde(default)]
    notes: Option<String>,
}

impl Check for RelationshipCreate {
    fn check(&self) -> Vec<FieldError> {
        if self.source_ci_id == self.target_ci_id {
            vec![FieldError {
                location: FieldLocation::Body,
                field: "targetCiId".into(),
                message: "A CI cannot be related to itself".into(),
                code: "custom".into(),
            }]
        } else {
            Vec::new()
        }
    }
}

// Endpoints are immutable: re-pointing an edge is a delete plus a create, which keeps history honest.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelationshipUpdate {
    #[schema(nullable = false)]
    relationship_type_id: Option<Uuid>,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    notes: Option<Option<String>>,
}

impl Check for RelationshipUpdate {
    fn check(&self) -> Vec<FieldError> {
        let mut c = ColumnSet::default();
        c.opt("relationship_type_id", self.relationship_type_id).opt("notes", self.notes.clone());
        non_empty(&c)
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

async fn load_row(conn: &mut PgConnection, id: Uuid) -> Result<RelationshipRow, AppError> {
    data::get(conn, id).await?.ok_or_else(|| AppError::missing("Relationship", id))
}

async fn load(conn: &mut PgConnection, id: Uuid) -> Result<Relationship, AppError> {
    load_row(conn, id).await.map(Relationship::from)
}

/// Relationships are read with view on both endpoints' classes and changed
/// with edit on the source's class (plus view on the target's).
fn require_endpoints(
    ctx: &RequestContext,
    source_class: Uuid,
    target_class: Uuid,
    op: ClassOp,
) -> Result<(), AppError> {
    ctx.require_class(source_class, op)?;
    ctx.require_class(target_class, ClassOp::View)
}

/// Only edges whose both endpoints are in classes the caller may view.
pub async fn list(pool: &PgPool, ctx: &RequestContext, q: &RelationshipList) -> Result<Page<Relationship>, AppError> {
    let visible = ctx.class_scope(ClassOp::View);
    let filter = |w: &mut Where<'_>| {
        if let Some(classes) = &visible {
            w.and()
                .push("s.class_id = ANY(")
                .push_bind(classes.clone())
                .push(") AND g.class_id = ANY(")
                .push_bind(classes.clone())
                .push(")");
        }
        if let Some(p) = q.deleted.predicate("r.deleted_at") {
            w.and_sql(&p);
        }
        if let Some(ids) = &q.ci_id {
            w.and()
                .push("(r.source_ci_id = ANY(")
                .push_bind(ids.0.clone())
                .push(") OR r.target_ci_id = ANY(")
                .push_bind(ids.0.clone())
                .push("))");
        }
        for (column, ids) in [
            ("r.source_ci_id", &q.source_ci_id),
            ("r.target_ci_id", &q.target_ci_id),
            ("r.relationship_type_id", &q.relationship_type_id),
        ] {
            if let Some(ids) = ids {
                w.and().push(column).push(" = ANY(").push_bind(ids.0.clone()).push(")");
            }
        }
        if let Some(text) = &q.q {
            data::search(w, &like_pattern(text));
        }
    };
    let (rows, total) = data::list(pool, &filter, &q.sort.field, q.sort.dir(), q.limit, q.offset).await?;
    Ok(Page { data: rows.into_iter().map(Relationship::from).collect(), page: q.page_meta(total) })
}

pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<Relationship, AppError> {
    let row = load_row(&mut *pool.acquire().await?, id).await?;
    require_endpoints(ctx, row.source_class_id, row.target_class_id, ClassOp::View)?;
    Ok(row.into())
}

pub async fn create(pool: &PgPool, ctx: &RequestContext, input: &RelationshipCreate) -> Result<Relationship, AppError> {
    let mut tx = pool.begin().await?;
    // Missing endpoints get a precise field error here; everything else
    // (duplicates, endpoint class rules, deleted CIs) is enforced by the
    // database and its errors map to field-level 400/409s.
    let found = data::existing_items(&mut tx, &[input.source_ci_id, input.target_ci_id]).await?;
    let class_of = |ci: Uuid| found.iter().find(|(id, _)| *id == ci).map(|(_, class)| *class);
    let missing: Vec<FieldError> = [("sourceCiId", input.source_ci_id), ("targetCiId", input.target_ci_id)]
        .into_iter()
        .filter(|(_, id)| class_of(*id).is_none())
        .map(|(field, _)| FieldError {
            location: FieldLocation::Body,
            field: field.into(),
            message: "Configuration item does not exist".into(),
            code: "not_found".into(),
        })
        .collect();
    if let (Some(source_class), Some(target_class)) = (class_of(input.source_ci_id), class_of(input.target_ci_id)) {
        require_endpoints(ctx, source_class, target_class, ClassOp::Edit)?;
    }
    if !missing.is_empty() {
        return Err(AppError::validation(missing));
    }
    let id = data::insert(
        &mut tx,
        input.relationship_type_id,
        input.source_ci_id,
        input.target_ci_id,
        input.notes.as_deref(),
    )
    .await?;
    let dto = load(&mut tx, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: "ci_relationships",
        entity_id: id,
        old_value: None,
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(dto)
}

pub async fn update(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    input: &RelationshipUpdate,
) -> Result<Relationship, AppError> {
    let mut tx = pool.begin().await?;
    match data::lock(&mut tx, id).await? {
        None => return Err(AppError::missing("Relationship", id)),
        Some(Some(_)) => return Err(AppError::conflict("This relationship was removed and cannot be modified")),
        Some(None) => {}
    }
    let row = load_row(&mut tx, id).await?;
    require_endpoints(ctx, row.source_class_id, row.target_class_id, ClassOp::Edit)?;
    let before = Relationship::from(row);
    data::update(&mut tx, id, input.relationship_type_id, input.notes.as_ref().map(|n| n.as_deref())).await?;
    let dto = load(&mut tx, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: "ci_relationships",
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(dto)
}

pub async fn remove(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    if !matches!(data::lock(&mut tx, id).await?, Some(None)) {
        return Err(AppError::missing("Relationship", id));
    }
    let row = load_row(&mut tx, id).await?;
    require_endpoints(ctx, row.source_class_id, row.target_class_id, ClassOp::Edit)?;
    let before = Relationship::from(row);
    data::soft_delete(&mut tx, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: "ci_relationships",
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: None,
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

const TAG: &str = "Relationships";
const BASE: &str = "/api/v1/relationships";
const BY_ID: &str = "/api/v1/relationships/{id}";

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, BASE, "listRelationships")
            .tag(TAG)
            .summary("List relationships (paginated, filterable by CI, direction and type)")
            .description("`q` matches source/target CI name, type name and notes. Use `ciId` for all edges of a CI. Only edges whose both CIs are in classes the caller may view.")
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<RelationshipList>, NoBody>| async move {
                Ok(Json(list(&api.pool, &api.ctx, &q).await?))
            }),
        route(Method::GET, BY_ID, "getRelationship")
            .tag(TAG)
            .summary("Get one relationship")
            .description("Needs view on both CIs' classes.")
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(get(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::POST, BASE, "createRelationship")
            .tag(TAG)
            .summary("Create a typed, directional relationship between two CIs")
            .description(
                "Rejected with 400 when the type does not allow these CI classes, when source equals target, or when a CI is deleted; 409 when the same live edge (or, for symmetric types, its reverse) exists. Needs edit on the source CI's class and view on the target's.",
            )
            .status(StatusCode::CREATED)
            .errors(&[ErrorCode::Conflict])
            .class_checked()
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<RelationshipCreate>>| async move {
                Ok(Json(create(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::PATCH, BY_ID, "updateRelationship")
            .tag(TAG)
            .summary("Update notes or type of a relationship (endpoints are immutable)")
            .description("Needs edit on the source CI's class and view on the target's.")
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<RelationshipUpdate>>| async move {
                Ok(Json(update(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, BY_ID, "deleteRelationship")
            .tag(TAG)
            .summary("Remove a relationship (soft delete; the same edge can be created again later)")
            .description("Needs edit on the source CI's class and view on the target's.")
            .errors(&[ErrorCode::NotFound])
            .class_checked()
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                remove(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
    ]
}
