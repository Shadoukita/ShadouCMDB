//! SQL for business services (SHAA-927): services are CIs of the class with
//! `system_role = 'business_service'`, members are live `ci_relationships` of
//! the type with `system_role = 'business_service_member'` (service = source,
//! member = target), owners are rows of `business_service_owners`.
//!
//! Every count and list of members takes the classes the caller may view
//! (`visible`, `None` = every class) and leaves the others out in SQL, so no
//! number or page says that a hidden member exists.

use chrono::{DateTime, Utc};
use sqlx::{AssertSqlSafe, PgConnection, Postgres, QueryBuilder};
use uuid::Uuid;

use super::crud::Where;
use super::items::{ItemFilters, SUMMARY_FROM, SummaryRow, push_filters, summary_columns};
use crate::api::schemas::like_pattern;

/// The built-in class and relationship type (migration 0033).
#[derive(Debug, Clone, Copy)]
pub struct Roles {
    pub service_class: Uuid,
    pub member_type: Uuid,
}

pub async fn roles(conn: &mut PgConnection) -> sqlx::Result<Option<Roles>> {
    let row: (Option<Uuid>, Option<Uuid>) = sqlx::query_as(
        "SELECT (SELECT id FROM cmdb.ci_classes WHERE system_role = 'business_service'),
                (SELECT id FROM cmdb.relationship_types WHERE system_role = 'business_service_member')",
    )
    .fetch_one(conn)
    .await?;
    Ok(match row {
        (Some(service_class), Some(member_type)) => Some(Roles { service_class, member_type }),
        _ => None,
    })
}

/// `AND <target CI> is in a class the caller may view`, for a member join aliased `m`.
fn push_visible(qb: &mut QueryBuilder<Postgres>, visible: Option<&[Uuid]>) {
    if let Some(v) = visible {
        qb.push(" AND m.class_id = ANY(").push_bind(v.to_vec()).push(")");
    }
}

/// Live member edges of the service whose member is live (and visible).
fn push_member_edges(qb: &mut QueryBuilder<Postgres>, roles: Roles, visible: Option<&[Uuid]>) {
    qb.push(" FROM cmdb.ci_relationships e JOIN cmdb.configuration_items m ON m.id = e.target_ci_id AND m.deleted_at IS NULL")
        .push(" WHERE e.relationship_type_id = ")
        .push_bind(roles.member_type)
        .push(" AND e.deleted_at IS NULL");
    push_visible(qb, visible);
}

// ---------------------------------------------------------------------------
// Services
// ---------------------------------------------------------------------------

/// A service: its CI summary and its visible member counts.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ServiceRow {
    #[sqlx(flatten)]
    pub ci: SummaryRow,
    pub member_count: i64,
    pub service_member_count: i64,
}

/// `<summary columns>, member_count, service_member_count FROM <summary> LEFT JOIN LATERAL (counts)`.
fn push_service_select(qb: &mut QueryBuilder<Postgres>, roles: Roles, visible: Option<&[Uuid]>) {
    qb.push(format!(
        "SELECT {}, mc.member_count, mc.service_member_count FROM {SUMMARY_FROM} LEFT JOIN LATERAL (
           SELECT count(*) AS member_count, count(*) FILTER (WHERE m.class_id = ",
        summary_columns()
    ))
    .push_bind(roles.service_class)
    .push(") AS service_member_count");
    push_member_edges(qb, roles, visible);
    qb.push(" AND e.source_ci_id = ci.id) mc ON true");
}

/// Live services among `ids`, with their counts (any order).
pub async fn services(
    conn: &mut PgConnection,
    roles: Roles,
    visible: Option<&[Uuid]>,
    ids: &[Uuid],
) -> sqlx::Result<Vec<ServiceRow>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb = QueryBuilder::new("");
    push_service_select(&mut qb, roles, visible);
    qb.push(" WHERE ci.deleted_at IS NULL AND ci.class_id = ")
        .push_bind(roles.service_class)
        .push(" AND ci.id = ANY(")
        .push_bind(ids.to_vec())
        .push(")");
    qb.build_query_as().fetch_all(conn).await
}

/// Who may appear in the owner filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerRole {
    Technical,
    Business,
}

impl OwnerRole {
    pub fn as_str(self) -> &'static str {
        match self {
            OwnerRole::Technical => "technical",
            OwnerRole::Business => "business",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerState {
    /// No owner in either role.
    None,
    /// At least one owner is a disabled user.
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceSort {
    Name,
    Criticality,
    MemberCount,
    UpdatedAt,
}

#[derive(Debug, Clone, Default)]
pub struct ServiceFilters {
    /// The CI list's search and validity filters (classes set to the service class).
    pub items: ItemFilters,
    /// Criticality values; `with_none` also selects services without one.
    pub criticality: Option<(Vec<Uuid>, bool)>,
    pub owner_ids: Option<Vec<Uuid>>,
    pub owner_role: Option<OwnerRole>,
    /// Services the user owns, directly or through one of their groups.
    pub mine: Option<Uuid>,
    /// `mine` asked by a caller who is not a user: nothing matches.
    pub mine_nobody: bool,
    pub owner_state: Option<OwnerState>,
}

fn push_service_filters(w: &mut Where<'_>, f: &ServiceFilters) {
    push_filters(w, &f.items);
    if let Some((ids, with_none)) = &f.criticality {
        let qb = w.and();
        qb.push("(ci.criticality_value_id = ANY(").push_bind(ids.clone()).push(")");
        if *with_none {
            qb.push(" OR ci.criticality_value_id IS NULL");
        }
        qb.push(")");
    }
    if let Some(ids) = &f.owner_ids {
        let qb = w.and();
        qb.push(
            "EXISTS (SELECT 1 FROM cmdb.business_service_owners o WHERE o.service_ci_id = ci.id AND (o.user_id = ANY(",
        )
        .push_bind(ids.clone())
        .push(") OR o.group_id = ANY(")
        .push_bind(ids.clone())
        .push("))");
        if let Some(role) = f.owner_role {
            qb.push(" AND o.role = ").push_bind(role.as_str());
        }
        qb.push(")");
    }
    if f.mine_nobody {
        w.and_sql("false");
    }
    if let Some(user) = f.mine {
        w.and()
            .push(
                "EXISTS (SELECT 1 FROM cmdb.business_service_owners o WHERE o.service_ci_id = ci.id AND (o.user_id = ",
            )
            .push_bind(user)
            .push(" OR o.group_id IN (SELECT gm.group_id FROM cmdb.user_group_members gm WHERE gm.user_id = ")
            .push_bind(user)
            .push(")))");
    }
    match f.owner_state {
        Some(OwnerState::None) => {
            w.and_sql("NOT EXISTS (SELECT 1 FROM cmdb.business_service_owners o WHERE o.service_ci_id = ci.id)")
        }
        Some(OwnerState::Disabled) => w.and_sql(
            "EXISTS (SELECT 1 FROM cmdb.business_service_owners o JOIN cmdb.users u ON u.id = o.user_id \
             WHERE o.service_ci_id = ci.id AND NOT u.is_active)",
        ),
        None => {}
    }
}

fn service_order(sort: ServiceSort, desc: bool) -> String {
    let dir = if desc { "DESC" } else { "ASC" };
    let first = match sort {
        ServiceSort::Name => return format!("lower(ci.label) {dir}, ci.label {dir}, ci.id ASC"),
        ServiceSort::Criticality => format!("crit.sort_order {dir} NULLS LAST, crit.key {dir}"),
        ServiceSort::MemberCount => format!("mc.member_count {dir}"),
        ServiceSort::UpdatedAt => format!("ci.updated_at {dir}"),
    };
    format!("{first}, lower(ci.label) ASC, ci.label ASC, ci.id ASC")
}

#[allow(clippy::too_many_arguments)]
fn push_service_page(
    qb: &mut QueryBuilder<Postgres>,
    roles: Roles,
    visible: Option<&[Uuid]>,
    f: &ServiceFilters,
    sort: ServiceSort,
    desc: bool,
    limit: i64,
    offset: i64,
) {
    push_service_select(qb, roles, visible);
    push_service_filters(&mut Where::new(qb), f);
    qb.push(format!(" ORDER BY {} LIMIT ", service_order(sort, desc)))
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);
}

/// The plan of a service list page (performance checks).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub async fn explain_services(
    conn: &mut PgConnection,
    roles: Roles,
    visible: Option<&[Uuid]>,
    f: &ServiceFilters,
    sort: ServiceSort,
    desc: bool,
    limit: i64,
) -> sqlx::Result<serde_json::Value> {
    let mut qb = QueryBuilder::new("EXPLAIN (FORMAT JSON) ");
    push_service_page(&mut qb, roles, visible, f, sort, desc, limit, 0);
    qb.build_query_scalar().fetch_one(conn).await
}

/// The plan of a member list page (performance checks).
#[cfg(test)]
pub async fn explain_members(
    conn: &mut PgConnection,
    roles: Roles,
    service: Uuid,
    visible: Option<&[Uuid]>,
    limit: i64,
    offset: i64,
) -> sqlx::Result<serde_json::Value> {
    let mut qb = QueryBuilder::new(format!(
        "EXPLAIN (FORMAT JSON) SELECT e.id AS membership_id, e.created_at AS added_at, {}",
        summary_columns()
    ));
    push_member_from(&mut qb, roles, service, visible);
    qb.push(format!(" ORDER BY {} LIMIT ", member_order(MemberSort::Name, false)))
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);
    qb.build_query_scalar().fetch_one(conn).await
}

/// One page of live services and the total matching the filters.
#[allow(clippy::too_many_arguments)]
pub async fn list_services(
    conn: &mut PgConnection,
    roles: Roles,
    visible: Option<&[Uuid]>,
    f: &ServiceFilters,
    sort: ServiceSort,
    desc: bool,
    limit: i64,
    offset: i64,
) -> sqlx::Result<(Vec<ServiceRow>, i64)> {
    let mut rows = QueryBuilder::new("");
    push_service_page(&mut rows, roles, visible, f, sort, desc, limit, offset);
    let page = rows.build_query_as::<ServiceRow>().fetch_all(&mut *conn).await?;

    let mut count = QueryBuilder::new("SELECT count(*) FROM cmdb.configuration_items ci");
    push_service_filters(&mut Where::new(&mut count), f);
    let total = count.build_query_scalar::<i64>().fetch_one(&mut *conn).await?;
    Ok((page, total))
}

/// Locks a service CI row (owner changes, member changes); its class, version
/// and deletion. FOR NO KEY UPDATE: it serialises the changes of one service
/// but not the foreign-key checks of an edge that names the service as a
/// member, so two services adding each other cannot deadlock.
pub async fn lock_ci(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<(Uuid, i32, Option<DateTime<Utc>>)>> {
    sqlx::query_as("SELECT class_id, version, deleted_at FROM cmdb.configuration_items WHERE id = $1 FOR NO KEY UPDATE")
        .bind(id)
        .fetch_optional(conn)
        .await
}

/// Bumps the CI's optimistic-locking version (owners are part of the service).
pub async fn bump_version(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<i32> {
    sqlx::query_scalar("UPDATE cmdb.configuration_items SET version = version + 1 WHERE id = $1 RETURNING version")
        .bind(id)
        .fetch_one(conn)
        .await
}

// ---------------------------------------------------------------------------
// Owners
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OwnerRow {
    pub service_ci_id: Uuid,
    pub role: String,
    pub user_id: Option<Uuid>,
    pub group_id: Option<Uuid>,
    /// users.display_name or user_groups.name
    pub display_name: String,
    /// users.is_active; true for groups
    pub active: bool,
}

/// The owners of these services, by service, role and position.
pub async fn owners(conn: &mut PgConnection, services: &[Uuid]) -> sqlx::Result<Vec<OwnerRow>> {
    if services.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as(
        "SELECT o.service_ci_id, o.role, o.user_id, o.group_id,
                coalesce(u.display_name, g.name) AS display_name, coalesce(u.is_active, true) AS active
         FROM cmdb.business_service_owners o
         LEFT JOIN cmdb.users u ON u.id = o.user_id
         LEFT JOIN cmdb.user_groups g ON g.id = o.group_id
         WHERE o.service_ci_id = ANY($1)
         ORDER BY o.service_ci_id, o.role, o.position",
    )
    .bind(services)
    .fetch_all(conn)
    .await
}

/// Of these ids, the users (Some(true)) and groups (Some(false)) that exist.
pub async fn existing_principals(
    conn: &mut PgConnection,
    users: &[Uuid],
    groups: &[Uuid],
) -> sqlx::Result<(Vec<Uuid>, Vec<Uuid>)> {
    let users: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM cmdb.users WHERE id = ANY($1)").bind(users).fetch_all(&mut *conn).await?;
    let groups: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM cmdb.user_groups WHERE id = ANY($1)").bind(groups).fetch_all(conn).await?;
    Ok((users, groups))
}

/// Replaces every owner of the service: `(role, user, group)` in display order per role.
pub async fn replace_owners(
    conn: &mut PgConnection,
    service: Uuid,
    owners: &[(&str, Option<Uuid>, Option<Uuid>)],
) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM cmdb.business_service_owners WHERE service_ci_id = $1")
        .bind(service)
        .execute(&mut *conn)
        .await?;
    if owners.is_empty() {
        return Ok(());
    }
    let mut position = std::collections::HashMap::<&str, i32>::new();
    let (mut roles, mut users, mut groups, mut positions) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (role, user, group) in owners {
        let p = position.entry(role).or_insert(0);
        roles.push(role.to_string());
        users.push(*user);
        groups.push(*group);
        positions.push(*p);
        *p += 1;
    }
    sqlx::query(
        "INSERT INTO cmdb.business_service_owners (service_ci_id, role, user_id, group_id, position)
         SELECT $1, u.role, u.user_id, u.group_id, u.position
         FROM UNNEST($2::text[], $3::uuid[], $4::uuid[], $5::int[]) AS u(role, user_id, group_id, position)",
    )
    .bind(service)
    .bind(roles)
    .bind(users)
    .bind(groups)
    .bind(positions)
    .execute(conn)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Members
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MemberRow {
    pub membership_id: Uuid,
    pub added_at: DateTime<Utc>,
    #[sqlx(flatten)]
    pub ci: SummaryRow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberSort {
    Name,
    Class,
    Criticality,
    AddedAt,
}

#[derive(Debug, Clone, Default)]
pub struct MemberFilters {
    pub q: Option<String>,
    pub class_ids: Option<Vec<Uuid>>,
    /// Some(true): only services; Some(false): only other CIs.
    pub services_only: Option<bool>,
    pub ci_ids: Option<Vec<Uuid>>,
}

fn push_member_from(qb: &mut QueryBuilder<Postgres>, roles: Roles, service: Uuid, visible: Option<&[Uuid]>) {
    qb.push(format!(
        " FROM {SUMMARY_FROM} JOIN cmdb.ci_relationships e ON e.target_ci_id = ci.id AND e.deleted_at IS NULL \
         AND e.relationship_type_id = "
    ))
    .push_bind(roles.member_type)
    .push(" AND e.source_ci_id = ")
    .push_bind(service)
    .push(" WHERE ci.deleted_at IS NULL");
    if let Some(v) = visible {
        qb.push(" AND ci.class_id = ANY(").push_bind(v.to_vec()).push(")");
    }
}

fn push_member_filters(qb: &mut QueryBuilder<Postgres>, roles: Roles, f: &MemberFilters) {
    if let Some(q) = &f.q {
        let pattern = like_pattern(q);
        qb.push(" AND (ci.label ILIKE ")
            .push_bind(pattern.clone())
            .push(" OR ci.ident ILIKE ")
            .push_bind(pattern)
            .push(")");
    }
    if let Some(ids) = &f.class_ids {
        qb.push(" AND ci.class_id = ANY(").push_bind(ids.clone()).push(")");
    }
    match f.services_only {
        Some(true) => {
            qb.push(" AND ci.class_id = ").push_bind(roles.service_class);
        }
        Some(false) => {
            qb.push(" AND ci.class_id <> ").push_bind(roles.service_class);
        }
        None => {}
    }
    if let Some(ids) = &f.ci_ids {
        qb.push(" AND ci.id = ANY(").push_bind(ids.clone()).push(")");
    }
}

fn member_order(sort: MemberSort, desc: bool) -> String {
    let dir = if desc { "DESC" } else { "ASC" };
    let first = match sort {
        MemberSort::Name => return format!("lower(ci.label) {dir}, ci.label {dir}, ci.id ASC"),
        MemberSort::Class => format!("lower(cls.name) {dir}"),
        MemberSort::Criticality => format!("crit.sort_order {dir} NULLS LAST, crit.key {dir}"),
        MemberSort::AddedAt => format!("e.created_at {dir}"),
    };
    format!("{first}, lower(ci.label) ASC, ci.label ASC, ci.id ASC")
}

/// Visible live members of a service, one page (`limit` None: all, for the
/// export) and the total.
#[allow(clippy::too_many_arguments)]
pub async fn list_members(
    conn: &mut PgConnection,
    roles: Roles,
    service: Uuid,
    visible: Option<&[Uuid]>,
    f: &MemberFilters,
    sort: MemberSort,
    desc: bool,
    limit: i64,
    offset: i64,
) -> sqlx::Result<(Vec<MemberRow>, i64)> {
    let mut rows =
        QueryBuilder::new(format!("SELECT e.id AS membership_id, e.created_at AS added_at, {}", summary_columns()));
    push_member_from(&mut rows, roles, service, visible);
    push_member_filters(&mut rows, roles, f);
    rows.push(format!(" ORDER BY {} LIMIT ", member_order(sort, desc)))
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);
    let page = rows.build_query_as::<MemberRow>().fetch_all(&mut *conn).await?;

    let mut count = QueryBuilder::new("SELECT count(*)");
    push_member_from(&mut count, roles, service, visible);
    push_member_filters(&mut count, roles, f);
    let total = count.build_query_scalar::<i64>().fetch_one(&mut *conn).await?;
    Ok((page, total))
}

/// The visible live members of a service (the member limit counts these).
pub async fn visible_member_count(
    conn: &mut PgConnection,
    roles: Roles,
    service: Uuid,
    visible: Option<&[Uuid]>,
) -> sqlx::Result<i64> {
    let mut qb = QueryBuilder::new("SELECT count(*)");
    push_member_edges(&mut qb, roles, visible);
    qb.push(" AND e.source_ci_id = ").push_bind(service);
    qb.build_query_scalar().fetch_one(conn).await
}

/// Of `targets`, those the service already includes (live edge).
pub async fn current_members(
    conn: &mut PgConnection,
    roles: Roles,
    service: Uuid,
    targets: &[Uuid],
) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT target_ci_id FROM cmdb.ci_relationships
         WHERE source_ci_id = $1 AND relationship_type_id = $2 AND deleted_at IS NULL AND target_ci_id = ANY($3)",
    )
    .bind(service)
    .bind(roles.member_type)
    .bind(targets)
    .fetch_all(conn)
    .await
}

/// Of these CIs, the live ones with their class.
pub async fn live_items(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<(Uuid, Uuid)>> {
    sqlx::query_as("SELECT id, class_id FROM cmdb.configuration_items WHERE id = ANY($1) AND deleted_at IS NULL")
        .bind(ids)
        .fetch_all(conn)
        .await
}

/// Takes the lock the membership trigger takes for service-in-service edges
/// (migration 0033), so the checks below read what the insert will see.
pub async fn lock_nesting(conn: &mut PgConnection) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('shadoucmdb:business-service-membership'))")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Lowers the trigger's nesting limit for this transaction (BUSINESS_SERVICE_MAX_NESTING).
pub async fn set_max_nesting(conn: &mut PgConnection, max: i32) -> sqlx::Result<()> {
    sqlx::query("SELECT set_config('shadoucmdb.business_service_max_nesting', $1, true)")
        .bind(max.to_string())
        .execute(conn)
        .await?;
    Ok(())
}

/// Per candidate service `target`: whether it already includes `service`
/// (directly or nested), and the longest chain of services below it. The walks
/// are the trigger's (migration 0033), bounded one level past the ceiling.
pub async fn below(
    conn: &mut PgConnection,
    roles: Roles,
    service: Uuid,
    targets: &[Uuid],
) -> sqlx::Result<Vec<(Uuid, bool, i32)>> {
    sqlx::query_as(
        "WITH RECURSIVE down (root, ci, depth) AS (
           SELECT t, t, 0 FROM unnest($1::uuid[]) t
           UNION
           SELECT d.root, e.target_ci_id, d.depth + 1
           FROM down d
           JOIN cmdb.ci_relationships e ON e.source_ci_id = d.ci AND e.relationship_type_id = $2 AND e.deleted_at IS NULL
           JOIN cmdb.configuration_items m ON m.id = e.target_ci_id AND m.class_id = $3
           WHERE d.depth < 9 AND d.ci <> $4
         )
         SELECT root, bool_or(ci = $4), max(depth) FROM down GROUP BY root",
    )
    .bind(targets)
    .bind(roles.member_type)
    .bind(roles.service_class)
    .bind(service)
    .fetch_all(conn)
    .await
}

/// The longest chain of services including `service`, above it.
pub async fn above(conn: &mut PgConnection, roles: Roles, service: Uuid) -> sqlx::Result<i32> {
    sqlx::query_scalar(
        "WITH RECURSIVE up (ci, depth) AS (
           SELECT $1::uuid, 0
           UNION
           SELECT e.source_ci_id, u.depth + 1
           FROM up u
           JOIN cmdb.ci_relationships e ON e.target_ci_id = u.ci AND e.relationship_type_id = $2 AND e.deleted_at IS NULL
           WHERE u.depth < 9
         )
         SELECT max(depth) FROM up",
    )
    .bind(service)
    .bind(roles.member_type)
    .fetch_one(conn)
    .await
}

/// A member edge as stored, for the audit rows (the shape a CI delete records).
pub type EdgeRecord = super::items::EdgeRecord;

/// Inserts one member edge per target in one statement (the trigger checks each).
pub async fn insert_members(
    conn: &mut PgConnection,
    roles: Roles,
    service: Uuid,
    targets: &[Uuid],
) -> sqlx::Result<Vec<EdgeRecord>> {
    sqlx::query_as(
        "INSERT INTO cmdb.ci_relationships (relationship_type_id, source_ci_id, target_ci_id)
         SELECT $1, $2, t FROM unnest($3::uuid[]) WITH ORDINALITY AS u(t, n) ORDER BY n
         RETURNING id, relationship_type_id, source_ci_id, target_ci_id, notes, created_at, updated_at, deleted_at",
    )
    .bind(roles.member_type)
    .bind(service)
    .bind(targets)
    .fetch_all(conn)
    .await
}

/// Soft-deletes the member edges to these targets; returns them as they were.
pub async fn remove_members(
    conn: &mut PgConnection,
    roles: Roles,
    service: Uuid,
    targets: &[Uuid],
) -> sqlx::Result<Vec<EdgeRecord>> {
    sqlx::query_as(
        "UPDATE cmdb.ci_relationships SET deleted_at = now()
         WHERE source_ci_id = $1 AND relationship_type_id = $2 AND deleted_at IS NULL AND target_ci_id = ANY($3)
         RETURNING id, relationship_type_id, source_ci_id, target_ci_id, notes, created_at, updated_at,
                   NULL::timestamptz AS deleted_at",
    )
    .bind(service)
    .bind(roles.member_type)
    .bind(targets)
    .fetch_all(conn)
    .await
}

/// Members of these ids (rows for the response of an add).
pub async fn members_by_ids(
    conn: &mut PgConnection,
    roles: Roles,
    service: Uuid,
    targets: &[Uuid],
) -> sqlx::Result<Vec<MemberRow>> {
    let mut qb =
        QueryBuilder::new(format!("SELECT e.id AS membership_id, e.created_at AS added_at, {}", summary_columns()));
    push_member_from(&mut qb, roles, service, None);
    qb.push(" AND ci.id = ANY(").push_bind(targets.to_vec()).push(") ORDER BY lower(ci.label), ci.label, ci.id");
    qb.build_query_as().fetch_all(conn).await
}

/// Of these classes, those that exist.
pub async fn existing_classes(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar("SELECT id FROM cmdb.ci_classes WHERE id = ANY($1)").bind(ids).fetch_all(conn).await
}

// ---------------------------------------------------------------------------
// Principals (owner picker)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PrincipalRow {
    pub kind: String,
    pub id: Uuid,
    pub display_name: String,
    pub username: Option<String>,
    pub active: bool,
}

/// Users (display name or username) and groups (name) matching `q`; prefix
/// matches first, then by name. At most `limit`.
pub async fn search_principals(
    conn: &mut PgConnection,
    q: &str,
    users: bool,
    groups: bool,
    include_inactive: bool,
    limit: i64,
) -> sqlx::Result<Vec<PrincipalRow>> {
    let pattern = like_pattern(q);
    let prefix = format!("{}%", crate::api::schemas::escape_like(&q.to_lowercase()));
    sqlx::query_as(AssertSqlSafe(
        "SELECT kind, id, display_name, username, active FROM (
           SELECT 'user' AS kind, u.id, u.display_name, u.username, u.is_active AS active
           FROM cmdb.users u
           WHERE $1 AND (u.is_active OR $3) AND (u.display_name ILIKE $4 OR u.username ILIKE $4)
           UNION ALL
           SELECT 'group', g.id, g.name, NULL, true
           FROM cmdb.user_groups g
           WHERE $2 AND g.name ILIKE $4
         ) p
         ORDER BY (lower(display_name) LIKE $5 OR lower(coalesce(username, '')) LIKE $5) DESC,
                  lower(display_name), kind DESC, id
         LIMIT $6"
            .to_owned(),
    ))
    .bind(users)
    .bind(groups)
    .bind(include_inactive)
    .bind(pattern)
    .bind(prefix)
    .bind(limit)
    .fetch_all(conn)
    .await
}
