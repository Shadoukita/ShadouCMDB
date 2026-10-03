//! Business services (SHAA-927 §3, §4): lists, members, owners, the "part of"
//! view and the owner picker, with the visibility rules of §3.2.
//!
//! The service class is one class, so "may the caller see services" is the
//! view right on it (403 without it: that is about a class, not a record).
//! Members are of any class: every list, count, limit and error leaves the
//! members the caller may not view out, and a hidden CI id answers exactly
//! like an id that does not exist.

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::schemas::{
    BusinessService, BusinessServiceLimits, BusinessServiceList, BusinessServiceMemberList, BusinessServiceMembersAdd,
    BusinessServiceMembersAdded, BusinessServiceMembersRemove, BusinessServiceOwnersReplace, BusinessServiceQuery,
    BusinessServiceSettings, BusinessServiceSummary, CiRef, ConfigurationItemService, ConfigurationItemServiceList,
    Member, MemberKind, MemberQuery, OwnerRoleParam, OwnerStateParam, Principal, PrincipalInput, PrincipalKind,
    PrincipalList, PrincipalQuery, PrincipalRef, ServiceOwners, entry_error,
};
use super::{MAX_BATCH, MAX_OWNERS_PER_ROLE};
use crate::api::context::{RequestContext, forbidden};
use crate::api::schemas::{Deleted, Paged};
use crate::auth::permissions::{ClassOp, GlobalPermission};
use crate::config::BusinessServiceConfig;
use crate::data::business_services::{
    self as data, MemberFilters, MemberRow, MemberSort, OwnerRole, OwnerState, Roles, ServiceFilters, ServiceRow,
    ServiceSort,
};
use crate::data::classes as class_data;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::impact as impact_data;
use crate::data::items::{self as items_data, ActiveFilter, ItemFilters, SummaryRow};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::csv_safe;
use crate::modules::impact::ImpactState;
use crate::modules::impact::engine::{self, Options, Way};
use crate::modules::impact::schemas::Visibility;
use crate::modules::items::schemas::CriticalityRef;
use crate::schema::model::Model;

/// Most services the "part of" view returns.
pub const MAX_PART_OF: usize = 200;
/// Most principals one owner-picker lookup returns.
pub const MAX_PRINCIPALS: i64 = 20;

/// The "does not exist" detail of a member id: the same text for a missing,
/// deleted or hidden CI (§3.2).
const NOT_FOUND: &str = "Configuration item does not exist";

async fn roles(conn: &mut PgConnection) -> Result<Roles, AppError> {
    configured(data::roles(conn).await?)
}

fn configured(roles: Option<Roles>) -> Result<Roles, AppError> {
    roles.ok_or_else(|| {
        tracing::error!("the built-in business service class or member type is missing (migration 0033)");
        AppError::internal()
    })
}

fn visibility(visible: &Option<Vec<Uuid>>) -> Visibility {
    // From the caller's permissions only, never from the data.
    if visible.is_some() { Visibility::Restricted } else { Visibility::AllClasses }
}

fn may(ctx: &RequestContext, roles: Roles, op: ClassOp) -> bool {
    ctx.require_class(roles.service_class, op).is_ok()
}

fn require(ctx: &RequestContext, roles: Roles, op: ClassOp) -> Result<(), AppError> {
    ctx.require_class(roles.service_class, op).map_err(|e| match e.code {
        ErrorCode::Forbidden => {
            forbidden(format!("You do not have the {} permission on business services", op.as_str()))
        }
        _ => e,
    })
}

pub fn limits(cfg: BusinessServiceConfig) -> BusinessServiceLimits {
    BusinessServiceLimits {
        max_members: cfg.max_members,
        max_batch: MAX_BATCH as i64,
        max_nesting: cfg.max_nesting,
        max_owners_per_role: MAX_OWNERS_PER_ROLE as i64,
    }
}

fn criticality(r: &SummaryRow) -> Option<CriticalityRef> {
    match (r.criticality_id, &r.criticality_key, &r.criticality_name, r.criticality_rank) {
        (Some(id), Some(key), Some(name), Some(rank)) => {
            Some(CriticalityRef { id, key: key.clone(), name: name.clone(), rank })
        }
        _ => None,
    }
}

fn principal(o: &data::OwnerRow) -> PrincipalRef {
    let (kind, id) = match (o.user_id, o.group_id) {
        (Some(u), _) => (PrincipalKind::User, u),
        (None, Some(g)) => (PrincipalKind::Group, g),
        // num_nonnulls(user_id, group_id) = 1 holds in the table.
        (None, None) => (PrincipalKind::Group, Uuid::nil()),
    };
    PrincipalRef { kind, id, display_name: o.display_name.clone(), active: o.active }
}

async fn owners_of(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<HashMap<Uuid, ServiceOwners>> {
    let mut out: HashMap<Uuid, ServiceOwners> = HashMap::new();
    for o in data::owners(conn, ids).await? {
        let entry = out.entry(o.service_ci_id).or_default();
        match o.role.as_str() {
            "technical" => entry.technical.push(principal(&o)),
            _ => entry.business.push(principal(&o)),
        }
    }
    Ok(out)
}

/// The summaries of these rows, in their order.
async fn summaries(conn: &mut PgConnection, rows: Vec<ServiceRow>) -> sqlx::Result<Vec<BusinessServiceSummary>> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.ci.id).collect();
    let mut owners = owners_of(conn, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|r| BusinessServiceSummary {
            id: r.ci.id,
            ident: r.ci.ident.clone(),
            name: r.ci.label.clone(),
            criticality: criticality(&r.ci),
            active: r.ci.active,
            owners: owners.remove(&r.ci.id).unwrap_or_default(),
            member_count: r.member_count,
            service_member_count: r.service_member_count,
            updated_at: r.ci.updated_at,
            version: r.ci.version,
        })
        .collect())
}

/// A live business service, or 404 (missing, deleted or not a service: alike).
async fn load(
    conn: &mut PgConnection,
    roles: Roles,
    visible: Option<&[Uuid]>,
    id: Uuid,
) -> Result<ServiceRow, AppError> {
    data::services(conn, roles, visible, &[id])
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::missing("Business service", id))
}

fn query_error(field: &str, message: impl Into<String>, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Query, field: field.into(), message: message.into(), code: code.into() }
}

// ---------------------------------------------------------------------------
// List and detail
// ---------------------------------------------------------------------------

pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &BusinessServiceQuery,
) -> Result<BusinessServiceList, AppError> {
    let mut conn = pool.acquire().await?;
    let roles = roles(&mut conn).await?;
    require(ctx, roles, ClassOp::View)?;
    let visible = ctx.class_scope(ClassOp::View);

    let model = Model::load(&mut conn).await?;
    let lineage: Vec<_> = model.lineage(roles.service_class).iter().filter_map(|c| model.table(c.id)).collect();
    let search_tables = items_data::search_tables(&model).into_iter().filter(|t| lineage.contains(&t.table)).collect();
    let include_inactive = q.include_inactive.map(bool::from).unwrap_or(true);
    let mine = q.mine.map(bool::from).unwrap_or(false);
    let user = ctx.principal().map(|p| p.user_id);
    let role = q.owner_role.map(|r| match r {
        OwnerRoleParam::Technical => OwnerRole::Technical,
        OwnerRoleParam::Business => OwnerRole::Business,
    });
    let filters = ServiceFilters {
        items: ItemFilters {
            q: q.q.clone(),
            class_ids: Some(vec![roles.service_class]),
            active: if include_inactive { ActiveFilter::Any } else { ActiveFilter::Active },
            deleted: Some(Deleted::Exclude),
            search_tables,
            ..Default::default()
        },
        criticality: q.criticality_value_id.as_ref().map(|c| (c.ids.clone(), c.none)),
        owner_ids: q.owner_id.as_ref().map(|l| l.0.clone()),
        owner_role: role,
        mine: if mine { user } else { None },
        mine_nobody: mine && user.is_none(),
        owner_state: q.owner_state.map(|s| match s {
            OwnerStateParam::None => OwnerState::None,
            OwnerStateParam::Disabled => OwnerState::Disabled,
        }),
    };
    let sort = match q.sort.field.as_str() {
        "name" => ServiceSort::Name,
        "memberCount" => ServiceSort::MemberCount,
        "updatedAt" => ServiceSort::UpdatedAt,
        _ => ServiceSort::Criticality,
    };
    let members_in = class_data::asset_scope(&mut conn, visible.as_deref()).await?;
    let (rows, total) =
        data::list_services(&mut conn, roles, members_in.as_deref(), &filters, sort, q.sort.desc, q.limit, q.offset)
            .await?;
    Ok(BusinessServiceList {
        data: summaries(&mut conn, rows).await?,
        page: q.page_meta(total),
        visibility: visibility(&visible),
    })
}

pub async fn get(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: BusinessServiceConfig,
    id: Uuid,
) -> Result<BusinessService, AppError> {
    let mut conn = pool.acquire().await?;
    let roles = roles(&mut conn).await?;
    require(ctx, roles, ClassOp::View)?;
    let visible = ctx.class_scope(ClassOp::View);
    let members_in = class_data::asset_scope(&mut conn, visible.as_deref()).await?;
    let row = load(&mut conn, roles, members_in.as_deref(), id).await?;
    let summary = summaries(&mut conn, vec![row]).await?.pop().ok_or_else(AppError::internal)?;
    Ok(BusinessService::new(summary, roles.service_class, visibility(&visible), limits(cfg)))
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

fn member(roles: Roles, r: MemberRow) -> Member {
    Member {
        membership_id: r.membership_id,
        is_service: r.ci.class_id == roles.service_class,
        added_at: r.added_at,
        ci: CiRef {
            id: r.ci.id,
            ident: r.ci.ident.clone(),
            name: r.ci.label.clone(),
            class_id: r.ci.class_id,
            class_name: r.ci.class_name.clone(),
            criticality: criticality(&r.ci),
            active: r.ci.active,
        },
    }
}

/// The member filters; a class filter naming a class that does not exist or
/// that the caller may not view is refused, the same for both.
async fn member_filters(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    q: &MemberQuery,
) -> Result<(MemberFilters, MemberSort), AppError> {
    let class_ids = q.class_id.as_ref().map(|l| l.0.clone());
    if let Some(ids) = &class_ids {
        let existing = data::existing_classes(conn, ids).await?;
        let refused: Vec<FieldError> = ids
            .iter()
            .filter(|id| !existing.contains(id) || ctx.require_class(**id, ClassOp::View).is_err())
            .map(|id| query_error("classId", format!("CI class {id} not found"), "not_found"))
            .collect();
        if !refused.is_empty() {
            return Err(AppError::validation(refused));
        }
    }
    let sort = match q.sort.field.as_str() {
        "class" => MemberSort::Class,
        "criticality" => MemberSort::Criticality,
        "addedAt" => MemberSort::AddedAt,
        _ => MemberSort::Name,
    };
    Ok((
        MemberFilters {
            q: q.q.clone(),
            class_ids,
            services_only: q.kind.map(|k| k == MemberKind::Service),
            ci_ids: q.ci_id.as_ref().map(|l| l.0.clone()),
        },
        sort,
    ))
}

pub async fn members(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    q: &MemberQuery,
) -> Result<BusinessServiceMemberList, AppError> {
    let mut conn = pool.acquire().await?;
    let roles = roles(&mut conn).await?;
    require(ctx, roles, ClassOp::View)?;
    let visible = ctx.class_scope(ClassOp::View);
    let members_in = class_data::asset_scope(&mut conn, visible.as_deref()).await?;
    load(&mut conn, roles, members_in.as_deref(), id).await?;
    let (filters, sort) = member_filters(&mut conn, ctx, q).await?;
    let (rows, total) =
        data::list_members(&mut conn, roles, id, members_in.as_deref(), &filters, sort, q.sort.desc, q.limit, q.offset)
            .await?;
    Ok(BusinessServiceMemberList {
        data: rows.into_iter().map(|r| member(roles, r)).collect(),
        page: q.page_meta(total),
        visibility: visibility(&visible),
    })
}

/// The `update` row on the service for a membership change (§3.4): the ids
/// added and removed, so the change shows in the service's history. The audit
/// API drops the ids the reader may not view.
fn members_entry(service: Uuid, added: &[Uuid], removed: &[Uuid]) -> AuditEntry {
    // An update row holds both values; both carry the change.
    let change = json!({ "members": { "added": added, "removed": removed } });
    AuditEntry {
        action: AuditAction::Update,
        entity_type: "configuration_items",
        entity_id: service,
        old_value: Some(change.clone()),
        new_value: Some(change),
    }
}

fn edge_entries(edges: &[data::EdgeRecord], action: AuditAction) -> Vec<AuditEntry> {
    edges
        .iter()
        .map(|e| AuditEntry {
            action,
            entity_type: "ci_relationships",
            entity_id: e.id,
            old_value: (action == AuditAction::Delete).then(|| crud::json(e)),
            new_value: (action == AuditAction::Create).then(|| crud::json(e)),
        })
        .collect()
}

/// Locks the service row so member changes of one service run one at a time
/// (the member limit is counted inside the lock).
async fn lock_service(conn: &mut PgConnection, roles: Roles, id: Uuid) -> Result<i32, AppError> {
    match data::lock_ci(conn, id).await? {
        Some((class, version, None)) if class == roles.service_class => Ok(version),
        _ => Err(AppError::missing("Business service", id)),
    }
}

pub async fn add_members(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: BusinessServiceConfig,
    id: Uuid,
    input: &BusinessServiceMembersAdd,
) -> Result<BusinessServiceMembersAdded, AppError> {
    let mut tx = pool.begin().await?;
    let roles = roles(&mut tx).await?;
    require(ctx, roles, ClassOp::View)?;
    lock_service(&mut tx, roles, id).await?;
    require(ctx, roles, ClassOp::Edit)?;
    let visible = ctx.class_scope(ClassOp::View);
    let ids = &input.member_ids;

    // Which ids name a live CI the caller may view: the others "do not exist".
    let class_of: HashMap<Uuid, Uuid> = data::live_items(&mut tx, ids).await?.into_iter().collect();
    let shown = |ci: &Uuid| class_of.get(ci).is_some_and(|c| ctx.require_class(*c, ClassOp::View).is_ok());
    let already: HashSet<Uuid> = data::current_members(&mut tx, roles, id, ids).await?.into_iter().collect();
    let process = class_data::process_class_ids(&mut tx).await?;
    let is_process = |ci: &Uuid| class_of.get(ci).is_some_and(|c| process.contains(c));

    let mut errors = Vec::new();
    let mut new = Vec::new();
    let mut already_members = Vec::new();
    for (i, ci) in ids.iter().enumerate() {
        if !shown(ci) {
            errors.push(entry_error("memberIds", i, NOT_FOUND, "not_found"));
        } else if is_process(ci) {
            errors.push(entry_error(
                "memberIds",
                i,
                "A process record (for example a change request) cannot be a member of a business service",
                "membership_process",
            ));
        } else if *ci == id {
            errors.push(entry_error(
                "memberIds",
                i,
                "A business service cannot be a member of itself",
                "membership_self",
            ));
        } else if already.contains(ci) {
            already_members.push(*ci);
        } else {
            new.push((i, *ci));
        }
    }

    // Services among the new members: no loops, and chains within the limit.
    let nested: Vec<Uuid> =
        new.iter().filter(|(_, ci)| class_of.get(ci) == Some(&roles.service_class)).map(|(_, ci)| *ci).collect();
    if !nested.is_empty() {
        data::lock_nesting(&mut tx).await?;
        let above = data::above(&mut tx, roles, id).await?;
        let below: HashMap<Uuid, (bool, i32)> =
            data::below(&mut tx, roles, id, &nested).await?.into_iter().map(|(r, c, d)| (r, (c, d))).collect();
        for (i, ci) in &new {
            match below.get(ci) {
                Some((true, _)) => errors.push(entry_error(
                    "memberIds",
                    *i,
                    "This business service already includes the service you are adding to; adding it would create \
                     a loop",
                    "membership_cycle",
                )),
                Some((false, depth)) if above + 1 + depth > cfg.max_nesting => errors.push(entry_error(
                    "memberIds",
                    *i,
                    &format!(
                        "Business services can be nested at most {} {} deep",
                        cfg.max_nesting,
                        if cfg.max_nesting == 1 { "level" } else { "levels" }
                    ),
                    "membership_nesting_depth",
                )),
                _ => {}
            }
        }
    }

    // The limit counts the members the caller may view (D6), so it says
    // nothing about the members they may not.
    let members_in = class_data::asset_scope(&mut tx, visible.as_deref()).await?;
    let count = data::visible_member_count(&mut tx, roles, id, members_in.as_deref()).await?;
    if count + new.len() as i64 > cfg.max_members {
        errors.push(FieldError {
            location: FieldLocation::Body,
            field: "memberIds".into(),
            message: format!("A business service can have at most {} members", cfg.max_members),
            code: "member_limit".into(),
        });
    }
    if !errors.is_empty() {
        errors.sort_by_key(|e| e.field != "memberIds");
        return Err(AppError::validation(errors));
    }

    let targets: Vec<Uuid> = new.iter().map(|(_, ci)| *ci).collect();
    let mut added = Vec::new();
    if !targets.is_empty() {
        data::set_max_nesting(&mut tx, cfg.max_nesting).await?;
        let edges = data::insert_members(&mut tx, roles, id, &targets).await?;
        let mut entries = edge_entries(&edges, AuditAction::Create);
        entries.push(members_entry(id, &targets, &[]));
        crud::write_audit(&mut tx, ctx, entries).await?;
        added =
            data::members_by_ids(&mut tx, roles, id, &targets).await?.into_iter().map(|r| member(roles, r)).collect();
    }
    tx.commit().await?;
    Ok(BusinessServiceMembersAdded { added, already_members })
}

async fn remove_in(
    tx: &mut PgConnection,
    ctx: &RequestContext,
    roles: Roles,
    id: Uuid,
    targets: &[Uuid],
) -> Result<(), AppError> {
    let edges = data::remove_members(tx, roles, id, targets).await?;
    let mut entries = edge_entries(&edges, AuditAction::Delete);
    entries.push(members_entry(id, &[], targets));
    crud::write_audit(tx, ctx, entries).await?;
    Ok(())
}

/// The ids among `ids` that are members the caller may view.
async fn visible_members(
    tx: &mut PgConnection,
    ctx: &RequestContext,
    roles: Roles,
    id: Uuid,
    ids: &[Uuid],
) -> Result<HashSet<Uuid>, AppError> {
    let class_of: HashMap<Uuid, Uuid> = data::live_items(tx, ids).await?.into_iter().collect();
    Ok(data::current_members(tx, roles, id, ids)
        .await?
        .into_iter()
        .filter(|ci| class_of.get(ci).is_some_and(|c| ctx.require_class(*c, ClassOp::View).is_ok()))
        .collect())
}

pub async fn remove_members(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    input: &BusinessServiceMembersRemove,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let roles = roles(&mut tx).await?;
    require(ctx, roles, ClassOp::View)?;
    lock_service(&mut tx, roles, id).await?;
    require(ctx, roles, ClassOp::Edit)?;
    let members = visible_members(&mut tx, ctx, roles, id, &input.member_ids).await?;
    let errors: Vec<FieldError> = input
        .member_ids
        .iter()
        .enumerate()
        .filter(|(_, ci)| !members.contains(ci))
        .map(|(i, _)| entry_error("memberIds", i, "Not a member of this business service", "not_found"))
        .collect();
    if !errors.is_empty() {
        return Err(AppError::validation(errors));
    }
    remove_in(&mut tx, ctx, roles, id, &input.member_ids).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove_member(pool: &PgPool, ctx: &RequestContext, id: Uuid, ci: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let roles = roles(&mut tx).await?;
    require(ctx, roles, ClassOp::View)?;
    lock_service(&mut tx, roles, id).await?;
    require(ctx, roles, ClassOp::Edit)?;
    if !visible_members(&mut tx, ctx, roles, id, &[ci]).await?.contains(&ci) {
        return Err(AppError::missing("Member", ci));
    }
    remove_in(&mut tx, ctx, roles, id, &[ci]).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// CSV export
// ---------------------------------------------------------------------------

/// A CSV record through [`csv_safe`]: every cell quoted, formulas neutralised
/// (the impact export's rule).
fn row(cells: &[&str]) -> String {
    let mut line = String::new();
    csv_safe::write_record(&mut line, ',', cells.iter().copied());
    line
}

pub const CSV_COLUMNS: &[&str] =
    &["ci_id", "ident", "name", "class", "criticality", "is_service", "active", "added_at"];

fn ident_for_file(ident: &str) -> String {
    ident.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' { c } else { '_' }).collect()
}

/// The member list as CSV, with the filters, and one `export` audit row.
pub async fn export(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: BusinessServiceConfig,
    id: Uuid,
    q: MemberQuery,
) -> Result<(String, String), AppError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
    let roles = roles(&mut tx).await?;
    require(ctx, roles, ClassOp::View)?;
    let visible = ctx.class_scope(ClassOp::View);
    let members_in = class_data::asset_scope(&mut tx, visible.as_deref()).await?;
    let service = load(&mut tx, roles, members_in.as_deref(), id).await?;
    let (filters, sort) = member_filters(&mut tx, ctx, &q).await?;
    let (rows, total) =
        data::list_members(&mut tx, roles, id, members_in.as_deref(), &filters, sort, q.sort.desc, cfg.max_members, 0)
            .await?;
    // GH#514: the snapshot is read-only; the audit row goes in its own
    // transaction below, so an audited write committed meanwhile cannot make
    // the chain-head update a serialization failure.
    tx.commit().await?;

    let one_line = |s: &str| s.replace(['\r', '\n'], " ");
    let mut described = Vec::new();
    if let Some(text) = &filters.q {
        described.push(format!("q={}", one_line(text)));
    }
    if let Some(ids) = &filters.class_ids {
        described.push(format!("classId={}", ids.iter().map(Uuid::to_string).collect::<Vec<_>>().join(" ")));
    }
    if let Some(kind) = q.kind {
        described.push(format!("kind={}", if kind == MemberKind::Service { "service" } else { "ci" }));
    }
    if let Some(ids) = &filters.ci_ids {
        described.push(format!("ciId={}", ids.iter().map(Uuid::to_string).collect::<Vec<_>>().join(" ")));
    }
    described.push(format!("sort={}{}", if q.sort.desc { "-" } else { "" }, q.sort.field));
    let mut comment = format!(
        "# Members of business service {} ({}): {}",
        service.ci.ident,
        one_line(&service.ci.label),
        described.join(", ")
    );
    if total > rows.len() as i64 {
        comment.push_str(&format!("; the first {} of {total} members", rows.len()));
    }
    if visible.is_some() {
        comment.push_str("; members of classes you are not allowed to view are not listed");
    }
    let mut body = row(&[&comment]);
    body.push_str(&row(CSV_COLUMNS));
    for r in &rows {
        body.push_str(&row(&[
            &r.ci.id.to_string(),
            &r.ci.ident,
            &r.ci.label,
            &r.ci.class_name,
            r.ci.criticality_name.as_deref().unwrap_or(""),
            if r.ci.class_id == roles.service_class { "true" } else { "false" },
            if r.ci.active { "true" } else { "false" },
            &crate::api::schemas::iso(&r.added_at),
        ]));
    }

    let entry = AuditEntry {
        action: AuditAction::Export,
        entity_type: "configuration_items",
        entity_id: id,
        old_value: None,
        new_value: Some(json!({
            "kind": "business_service_members",
            "format": "csv",
            "rowCount": rows.len(),
            "visibility": visibility(&visible),
        })),
    };
    let mut tx = pool.begin().await?;
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    let name =
        format!("service-members-{}-{}.csv", ident_for_file(&service.ci.ident), Utc::now().format("%Y%m%d-%H%M"));
    Ok((name, body))
}

// ---------------------------------------------------------------------------
// Owners
// ---------------------------------------------------------------------------

/// The owners as recorded in the audit row: kind, id and name.
fn owners_json(o: &ServiceOwners) -> Value {
    let list = |l: &[PrincipalRef]| -> Vec<Value> {
        l.iter().map(|p| json!({ "kind": p.kind.as_str(), "id": p.id, "name": p.display_name })).collect()
    };
    json!({ "owners": { "technical": list(&o.technical), "business": list(&o.business) } })
}

pub async fn replace_owners(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    input: &BusinessServiceOwnersReplace,
) -> Result<ServiceOwners, AppError> {
    let mut tx = pool.begin().await?;
    let roles = roles(&mut tx).await?;
    require(ctx, roles, ClassOp::View)?;
    let version = lock_service(&mut tx, roles, id).await?;
    require(ctx, roles, ClassOp::Edit)?;

    let all = || input.technical.iter().chain(input.business.iter());
    let users: Vec<Uuid> = all().filter(|p| p.kind == PrincipalKind::User).map(|p| p.id).collect();
    let groups: Vec<Uuid> = all().filter(|p| p.kind == PrincipalKind::Group).map(|p| p.id).collect();
    let (users, groups) = data::existing_principals(&mut tx, &users, &groups).await?;
    let mut errors = Vec::new();
    for (field, list) in [("technical", &input.technical), ("business", &input.business)] {
        let mut seen: HashSet<(PrincipalKind, Uuid)> = HashSet::new();
        for (i, p) in list.iter().enumerate() {
            let exists = match p.kind {
                PrincipalKind::User => users.contains(&p.id),
                PrincipalKind::Group => groups.contains(&p.id),
            };
            if !seen.insert((p.kind, p.id)) {
                errors.push(entry_error(field, i, "Already an owner in this role", "duplicate"));
            } else if !exists {
                let what = if p.kind == PrincipalKind::User { "User" } else { "Group" };
                errors.push(entry_error(field, i, &format!("{what} does not exist"), "not_found"));
            }
        }
    }
    if !errors.is_empty() {
        return Err(AppError::validation(errors));
    }
    if input.version != version {
        return Err(AppError::new(
            ErrorCode::VersionConflict,
            format!(
                "The business service was changed by someone else (you sent version {}, current is {version}). \
                 Reload and retry.",
                input.version
            ),
        )
        .with_details(vec![FieldError {
            location: FieldLocation::Body,
            field: "version".into(),
            message: format!("Current version is {version}"),
            code: "stale".into(),
        }]));
    }

    let before = owners_of(&mut tx, &[id]).await?.remove(&id).unwrap_or_default();
    let rows: Vec<(&str, Option<Uuid>, Option<Uuid>)> =
        [("technical", &input.technical), ("business", &input.business)]
            .into_iter()
            .flat_map(|(role, list)| {
                list.iter().map(move |p: &PrincipalInput| match p.kind {
                    PrincipalKind::User => (role, Some(p.id), None),
                    PrincipalKind::Group => (role, None, Some(p.id)),
                })
            })
            .collect();
    data::replace_owners(&mut tx, id, &rows).await?;
    data::bump_version(&mut tx, id).await?;
    let after = owners_of(&mut tx, &[id]).await?.remove(&id).unwrap_or_default();
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: "configuration_items",
        entity_id: id,
        old_value: Some(owners_json(&before)),
        new_value: Some(owners_json(&after)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(after)
}

// ---------------------------------------------------------------------------
// "Part of business services"
// ---------------------------------------------------------------------------

/// The "part of" view could not be assembled within the impact analysis's
/// allowance after its deadline (a very slow database): answered rather than
/// holding the connection and the analysis place until the request times out
/// (SHAA-1112).
fn part_of_out_of_time() -> AppError {
    let mut err = AppError::new(
        ErrorCode::ServerBusy,
        "The business services of the configuration item could not be listed in time because the database is \
         responding slowly; retry shortly",
    );
    err.retry_after = Some(1);
    err
}

/// A statement's error, with one cancelled by the deadline as [`part_of_out_of_time`].
fn part_of_db_error(e: sqlx::Error) -> AppError {
    if engine::is_query_canceled(&e) { part_of_out_of_time() } else { e.into() }
}

/// Bounds the next statement on `conn` by `until`.
async fn bound(conn: &mut PgConnection, until: tokio::time::Instant) -> Result<(), AppError> {
    let ms = engine::remaining_ms(until).ok_or_else(part_of_out_of_time)?;
    impact_data::set_statement_timeout(conn, ms).await?;
    Ok(())
}

pub async fn part_of(
    pool: &PgPool,
    ctx: &RequestContext,
    impact: &std::sync::Arc<ImpactState>,
    cfg: BusinessServiceConfig,
    ci: Uuid,
) -> Result<ConfigurationItemServiceList, AppError> {
    let visible = ctx.class_scope(ClassOp::View);
    let empty = |visible: &Option<Vec<Uuid>>| ConfigurationItemServiceList {
        data: Vec::new(),
        truncated: false,
        visibility: visibility(visible),
    };
    // As an impact analysis (GH#393): the walk stops at IMPACT_TIMEOUT_MS from
    // the start of the request, and every statement, before and after it, ends
    // within the assembly allowance after that (SHAA-1112).
    let deadline = tokio::time::Instant::now() + impact.config.timeout;
    let assembly = engine::assembly_deadline(deadline);
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY").execute(&mut *tx).await?;
    bound(&mut tx, assembly).await?;
    match items_data::summary(&mut tx, ci).await.map_err(part_of_db_error)? {
        Some(r) if r.deleted_at.is_none() => ctx.require_class_visible(r.class_id, "Configuration item", ci)?,
        _ => return Err(AppError::missing("Configuration item", ci)),
    }
    bound(&mut tx, assembly).await?;
    let roles = configured(data::roles(&mut tx).await.map_err(part_of_db_error)?)?;
    if !may(ctx, roles, ClassOp::View) {
        return Ok(empty(&visible));
    }
    let _permit = impact.acquire(ctx)?;
    let types = [roles.member_type];
    let classes = [roles.service_class];
    let opts = Options {
        ways: &[Way::Downstream],
        depth: cfg.max_nesting + 1,
        types: Some(&types),
        include_inactive: true,
        max_nodes: MAX_PART_OF + 1,
        deadline,
        visible: visible.as_deref(),
        result_classes: Some(&classes),
    };
    let traversal = engine::traverse(&mut tx, &[ci], &opts).await?;
    let Some(walk) = traversal.walks.into_iter().next() else { return Ok(empty(&visible)) };
    let parent: HashMap<Uuid, Uuid> = walk.nodes.iter().map(|n| (n.id, n.via.parent_id)).collect();
    let mut nodes = walk.nodes;
    // A chain deeper than the limit (made before GH#410 closed the other paths
    // into membership) is listed up to the limit and reported as cut short.
    let truncated = nodes.len() > MAX_PART_OF
        || walk.more_beyond_depth
        || walk.truncated.is_some_and(|t| t != engine::Truncation::MaxNodes);
    nodes.truncate(MAX_PART_OF);
    let ids: Vec<Uuid> = nodes.iter().map(|n| n.id).collect();
    bound(&mut tx, assembly).await?;
    let members_in = class_data::asset_scope(&mut tx, visible.as_deref()).await.map_err(part_of_db_error)?;
    bound(&mut tx, assembly).await?;
    let rows = data::services(&mut tx, roles, members_in.as_deref(), &ids).await.map_err(part_of_db_error)?;
    bound(&mut tx, assembly).await?;
    let mut by_id: HashMap<Uuid, BusinessServiceSummary> =
        summaries(&mut tx, rows).await.map_err(part_of_db_error)?.into_iter().map(|s| (s.id, s)).collect();
    tx.commit().await?;

    let mut data = Vec::with_capacity(nodes.len());
    for n in &nodes {
        let Some(service) = by_id.remove(&n.id) else { continue };
        // Back from the service to the CI, then read from the CI outwards.
        let mut via = Vec::new();
        let mut at = n.via.parent_id;
        while at != ci && via.len() <= MAX_PART_OF {
            via.push(at);
            match parent.get(&at) {
                Some(p) => at = *p,
                None => break,
            }
        }
        via.reverse();
        data.push((n.hops, ConfigurationItemService { service, direct: n.hops == 1, via_service_ids: via }));
    }
    data.sort_by(|(ha, a), (hb, b)| {
        ha.cmp(hb)
            .then_with(|| a.service.name.to_lowercase().cmp(&b.service.name.to_lowercase()))
            .then_with(|| a.service.id.cmp(&b.service.id))
    });
    Ok(ConfigurationItemServiceList {
        data: data.into_iter().map(|(_, s)| s).collect(),
        truncated,
        visibility: visibility(&visible),
    })
}

// ---------------------------------------------------------------------------
// Owner picker and settings
// ---------------------------------------------------------------------------

pub async fn principals(pool: &PgPool, ctx: &RequestContext, q: &PrincipalQuery) -> Result<PrincipalList, AppError> {
    let mut conn = pool.acquire().await?;
    let roles = roles(&mut conn).await?;
    if !may(ctx, roles, ClassOp::Edit) && ctx.require(GlobalPermission::UsersManage).is_err() {
        return Err(forbidden(
            "Looking up owners needs the edit permission on business services or the users.manage permission",
        ));
    }
    let Some(text) = q.q.as_deref().map(str::trim) else {
        return Err(AppError::validation(vec![query_error("q", "Required", "required")]));
    };
    if text.chars().count() < 2 {
        return Err(AppError::validation(vec![query_error(
            "q",
            "Too small: expected string to have >=2 characters",
            "too_small",
        )]));
    }
    let rows = data::search_principals(
        &mut conn,
        text,
        q.kind != Some(PrincipalKind::Group),
        q.kind != Some(PrincipalKind::User),
        q.include_inactive.map(bool::from).unwrap_or(false),
        MAX_PRINCIPALS,
    )
    .await?;
    Ok(PrincipalList {
        data: rows
            .into_iter()
            .map(|r| Principal {
                kind: if r.kind == "user" { PrincipalKind::User } else { PrincipalKind::Group },
                id: r.id,
                display_name: r.display_name,
                username: r.username,
                active: r.active,
            })
            .collect(),
    })
}

pub async fn settings(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: BusinessServiceConfig,
) -> Result<BusinessServiceSettings, AppError> {
    let mut conn = pool.acquire().await?;
    let roles = roles(&mut conn).await?;
    Ok(BusinessServiceSettings {
        class_id: roles.service_class,
        member_relationship_type_id: roles.member_type,
        can_view: may(ctx, roles, ClassOp::View),
        can_edit: may(ctx, roles, ClassOp::Edit),
        limits: limits(cfg),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GH#388: the shared csv_safe rule, not a weaker local one.
    const FORMULAS: [&str; 14] = [
        "+1",
        "-1",
        "-2+3",
        "@SUM(A1)",
        " =1",
        "\u{a0}=1",
        "\u{3000}+1",
        "＝1",
        "＋1",
        "－1",
        "＠SUM(A1)",
        "\tx",
        "\rx",
        "\nx",
    ];

    #[test]
    fn cells_are_quoted_and_formulas_neutralised() {
        assert_eq!(row(&["web-01", "=HYPERLINK(\"x\")", ""]), "\"web-01\",\"'=HYPERLINK(\"\"x\"\")\",\"\"\r\n");
        for bad in FORMULAS {
            assert_eq!(row(&[bad]), format!("\"'{bad}\"\r\n"), "{bad:?}");
        }
        // A line break inside a value stays inside its quoted cell.
        assert_eq!(row(&["Rack A\nSlot 4", "x"]), "\"Rack A\nSlot 4\",\"x\"\r\n");
    }
}
