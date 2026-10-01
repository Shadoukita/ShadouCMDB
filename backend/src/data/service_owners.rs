//! Owners of business services (`business_service_owners`, migration 0034)
//! as the audit log records them, and the clean-up when a user or a user
//! group that owns services is deleted (SHAA-927 §1.4, §3.4).
//!
//! The owner rows go with the user or group (`ON DELETE CASCADE`). The delete
//! path writes one `update` row per affected service on `configuration_items`
//! (the service's history), with `{"owners": {"technical": [...], "business":
//! [...]}}` before and after, each entry `{kind, id, name}`. Soft-deleted
//! services are included: they keep their owners for a restore, so they lose
//! this one too.

use std::collections::HashMap;

use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::api::context::RequestContext;
use crate::data::crud::{self, AuditAction, AuditEntry};

/// Who owns: one user or one user group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Principal {
    User(Uuid),
    Group(Uuid),
}

impl Principal {
    fn column(self) -> &'static str {
        match self {
            Principal::User(_) => "user_id",
            Principal::Group(_) => "group_id",
        }
    }

    fn id(self) -> Uuid {
        match self {
            Principal::User(id) | Principal::Group(id) => id,
        }
    }
}

/// The class whose CIs are business services (exactly one, migration 0033).
pub async fn service_class_id(conn: &mut PgConnection) -> sqlx::Result<Uuid> {
    sqlx::query_scalar("SELECT id FROM ci_classes WHERE system_role = 'business_service'").fetch_one(conn).await
}

/// How many services (deleted ones included) the principal owns in any role.
pub async fn count_owned(conn: &mut PgConnection, who: Principal) -> sqlx::Result<i64> {
    // The column name is one of two constants.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(DISTINCT service_ci_id) FROM business_service_owners WHERE {} = $1",
        who.column()
    )))
    .bind(who.id())
    .fetch_one(conn)
    .await
}

/// (service, role, user, group, display name) of one owner row.
type OwnerRow = (Uuid, String, Option<Uuid>, Option<Uuid>, Option<String>);

/// The `{"owners": {...}}` audit value of each service, owners in display order.
pub async fn owners_values(conn: &mut PgConnection, services: &[Uuid]) -> sqlx::Result<HashMap<Uuid, Value>> {
    let rows: Vec<OwnerRow> = sqlx::query_as(
        "SELECT o.service_ci_id, o.role, o.user_id, o.group_id, coalesce(u.display_name, g.name)
         FROM business_service_owners o
         LEFT JOIN users u ON u.id = o.user_id
         LEFT JOIN user_groups g ON g.id = o.group_id
         WHERE o.service_ci_id = ANY($1)
         ORDER BY o.service_ci_id, o.role, o.position, o.id",
    )
    .bind(services)
    .fetch_all(conn)
    .await?;
    let mut by_service: HashMap<Uuid, (Vec<Value>, Vec<Value>)> =
        services.iter().map(|s| (*s, (Vec::new(), Vec::new()))).collect();
    for (service, role, user, group, name) in rows {
        let entry = match (user, group) {
            (Some(id), _) => json!({ "kind": "user", "id": id, "name": name }),
            (None, Some(id)) => json!({ "kind": "group", "id": id, "name": name }),
            (None, None) => continue,
        };
        let lists = by_service.entry(service).or_default();
        if role == "technical" { lists.0.push(entry) } else { lists.1.push(entry) }
    }
    Ok(by_service
        .into_iter()
        .map(|(s, (technical, business))| (s, json!({ "owners": { "technical": technical, "business": business } })))
        .collect())
}

/// The services a principal owns, locked against concurrent owner and service
/// changes, with their owners before the principal goes.
pub struct OwnerCleanup {
    services: Vec<Uuid>,
    before: HashMap<Uuid, Value>,
}

impl OwnerCleanup {
    /// Call before the user or group is deleted, and before the first audit
    /// row of the transaction (the audit chain head is taken last, GH#166).
    pub async fn prepare(conn: &mut PgConnection, who: Principal) -> sqlx::Result<Self> {
        let services: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT id FROM configuration_items
             WHERE id IN (SELECT service_ci_id FROM business_service_owners WHERE {} = $1)
             ORDER BY id FOR UPDATE",
            who.column()
        )))
        .bind(who.id())
        .fetch_all(&mut *conn)
        .await?;
        let before = owners_values(conn, &services).await?;
        Ok(OwnerCleanup { services, before })
    }

    /// Call after the delete: one `update` audit row per affected service.
    /// Returns how many services lost an owner.
    pub async fn finish(self, conn: &mut PgConnection, ctx: &RequestContext) -> sqlx::Result<i64> {
        let mut after = owners_values(conn, &self.services).await?;
        let mut before = self.before;
        let entries: Vec<AuditEntry> = self
            .services
            .iter()
            .filter_map(|s| {
                let (old, new) = (before.remove(s)?, after.remove(s)?);
                (old != new).then(|| AuditEntry {
                    action: AuditAction::Update,
                    entity_type: "configuration_items",
                    entity_id: *s,
                    old_value: Some(old),
                    new_value: Some(new),
                })
            })
            .collect();
        let n = entries.len() as i64;
        crud::write_audit(conn, ctx, entries).await?;
        Ok(n)
    }
}
