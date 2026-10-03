//! Migration 0041 (GH#410, SHAA-1389) against an install with business
//! services: the rules on the member type are removed and no others, members
//! and services are kept, each removed rule leaves a `delete` row in the
//! audit chain (GH#515), and afterwards the database refuses a rule on the
//! member type and a CI moving into the business service type.

use serde_json::{Value, json};
use sqlx::{Executor, PgPool};
use uuid::Uuid;

use super::upgrade_0033::v02x;
use crate::db::MIGRATOR;

/// The adopted service class (c2) includes the application (101) and Payroll
/// (202); a rule on the member type and one on depends_on exist.
const BEFORE: &str = "
INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id)
  SELECT id, '00000000-0000-4000-8000-0000000000c2', '00000000-0000-4000-8000-0000000000c1'
  FROM relationship_types WHERE system_role = 'business_service_member';
INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id)
  SELECT t.id, '00000000-0000-4000-8000-000000000201', m
  FROM relationship_types t,
       unnest(ARRAY['00000000-0000-4000-8000-000000000101', '00000000-0000-4000-8000-000000000202']::uuid[]) m
  WHERE t.system_role = 'business_service_member';
";

async fn rows(pool: &PgPool, sql: &str) -> Vec<String> {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_all(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

const RULES: &str = "SELECT t.key || ':' || r.source_class_id || '>' || r.target_class_id
  FROM relationship_type_rules r JOIN relationship_types t ON t.id = r.relationship_type_id ORDER BY 1";
const EDGES: &str = "SELECT source_ci_id || '>' || target_ci_id || ':' || relationship_type_id
  FROM ci_relationships WHERE deleted_at IS NULL ORDER BY 1";
const CLASSES: &str = "SELECT id || ':' || class_id FROM configuration_items ORDER BY 1";

fn constraint(res: Result<sqlx::postgres::PgQueryResult, sqlx::Error>) -> String {
    let err = res.expect_err("the statement should have been rejected");
    err.as_database_error().and_then(|e| e.constraint()).unwrap_or_else(|| panic!("{err}")).to_owned()
}

#[tokio::test]
async fn member_rules_are_removed_and_no_ci_becomes_a_service() {
    let Some(db) = v02x("member_rules_are_removed", true).await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(40, pool).await.expect("migrations up to 0040");
    pool.execute(BEFORE).await.expect("data before the upgrade");
    let (rules, edges, classes) = (rows(pool, RULES).await, rows(pool, EDGES).await, rows(pool, CLASSES).await);
    assert_eq!(rules.len(), 2, "{rules:?}");
    let member_rule_id: Uuid = sqlx::query_scalar(
        "SELECT r.id FROM relationship_type_rules r JOIN relationship_types t ON t.id = r.relationship_type_id
         WHERE t.system_role = 'business_service_member'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    MIGRATOR.run_to(41, pool).await.expect("migration 0041");

    let kept: Vec<String> = rules.into_iter().filter(|r| r.starts_with("depends_on:")).collect();
    assert_eq!(rows(pool, RULES).await, kept, "only the member type's rule is removed");
    assert_eq!(rows(pool, EDGES).await, edges, "members are kept");
    assert_eq!(rows(pool, CLASSES).await, classes);

    // The removed rule's history ends in a system `delete` naming the reason;
    // the kept rule has none, and the chain verifies through the new row.
    let audit: Vec<(String, Option<String>, String, Uuid, Value, Value)> = sqlx::query_as(
        "SELECT actor_type, actor_name, action, entity_id, old_value, new_value FROM audit_log
         WHERE entity_type = 'relationship_type_rules' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    assert_eq!(audit.len(), 1, "{audit:?}");
    let (actor_type, actor_name, action, entity_id, old, new) = &audit[0];
    assert_eq!(
        (actor_type.as_str(), actor_name.as_deref(), action.as_str(), *entity_id),
        ("system", Some("migration 0041"), "delete", member_rule_id)
    );
    assert_eq!(old["id"], json!(member_rule_id));
    assert_eq!(old["sourceClassId"], "00000000-0000-4000-8000-0000000000c2");
    assert_eq!(old["targetClassId"], "00000000-0000-4000-8000-0000000000c1");
    assert!(old["createdAt"].is_string() && old["relationshipTypeId"].is_string(), "{old}");
    assert_eq!(new["migration"], "0041");
    assert!(new["reason"].as_str().is_some_and(|r| r.contains("takes no rules")), "{new}");
    let problems: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log_verify()").fetch_one(pool).await.unwrap();
    assert_eq!(problems, 0, "the audit chain verifies");

    let member_rule = sqlx::query(
        "INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id)
         SELECT id, '00000000-0000-4000-8000-0000000000c2', '00000000-0000-4000-8000-0000000000c1'
         FROM relationship_types WHERE system_role = 'business_service_member'",
    );
    assert_eq!(constraint(member_rule.execute(pool).await), "relationship_type_rules_system_type");
    let retarget = sqlx::query(
        "UPDATE relationship_type_rules SET relationship_type_id =
           (SELECT id FROM relationship_types WHERE system_role = 'business_service_member')",
    );
    assert_eq!(constraint(retarget.execute(pool).await), "relationship_type_rules_system_type");

    // The application is a member; it cannot become a service. A service with
    // members keeps its type (0034), one without may still leave it.
    let to_service = "UPDATE configuration_items SET class_id = '00000000-0000-4000-8000-0000000000c2'
                      WHERE id = '00000000-0000-4000-8000-000000000101'";
    assert_eq!(constraint(sqlx::query(to_service).execute(pool).await), "configuration_items_service_class");
    let from_service = "UPDATE configuration_items SET class_id = '00000000-0000-4000-8000-0000000000c1'
                        WHERE id = '00000000-0000-4000-8000-000000000201'";
    assert_eq!(constraint(sqlx::query(from_service).execute(pool).await), "configuration_items_service_members");
    assert_eq!(rows(pool, CLASSES).await, classes);

    // Running the migrations again is a no-op.
    MIGRATOR.run(pool).await.expect("re-run");
    assert_eq!(rows(pool, RULES).await, kept);
    let deletes: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE actor_name = 'migration 0041'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(deletes, 1, "no second audit row");
    db.drop().await;
}

/// The upgrade replay for 0041 (SHAA-1435). The upgrade CI job
/// (.github/workflows/upgrade.yml) starts at v0.1.0-rc.1, before the member
/// type existed (0033), so it cannot reach this state. On top of `BEFORE`: a
/// second rule on the member type (service to service), a depends_on rule from
/// the application to the service class (it targets the service class but is
/// not on the member type), the application depending on the service directly,
/// two saved import mappings (one naming the member type) and an audit row per
/// rule written before the upgrade.
const MORE: &str = "
INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id)
  SELECT id, '00000000-0000-4000-8000-0000000000c2', '00000000-0000-4000-8000-0000000000c2'
  FROM relationship_types WHERE system_role = 'business_service_member';
INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id) VALUES
  ('00000000-0000-4000-8000-0000000000d1', '00000000-0000-4000-8000-0000000000c1', '00000000-0000-4000-8000-0000000000c2');
INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id) VALUES
  ('00000000-0000-4000-8000-0000000000d1', '00000000-0000-4000-8000-000000000101', '00000000-0000-4000-8000-000000000201');
INSERT INTO import_mappings (name, class_key, definition, created_by_name, updated_by_name) VALUES
  ('Members', 'service', '{\"mode\": \"update_only\", \"columns\": [{\"header\": \"Member\", \"target\": {\"kind\": \"relationship\",
    \"typeKey\": \"business_service_member\", \"direction\": \"outgoing\", \"match\": {\"by\": \"ident\"}}}]}', 'admin', 'admin'),
  ('Dependencies', 'application', '{\"mode\": \"update_only\", \"columns\": [{\"header\": \"Service\", \"target\": {\"kind\": \"relationship\",
    \"typeKey\": \"depends_on\", \"direction\": \"outgoing\", \"match\": {\"by\": \"ident\"}}}]}', 'admin', 'admin');
INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, new_value)
  SELECT 'user', 'admin', 'create', 'relationship_type_rules', id, '{}' FROM relationship_type_rules ORDER BY id;
";

const MAPPINGS: &str = "SELECT name || ':' || version || ':' || definition::text FROM import_mappings ORDER BY 1";
const AUDIT: &str = "SELECT chain_seq || ':' || encode(row_hash, 'hex') FROM audit_log ORDER BY chain_seq";
const MEMBER_RULE_IDS: &str = "SELECT r.id::text FROM relationship_type_rules r
  JOIN relationship_types t ON t.id = r.relationship_type_id WHERE t.system_role = 'business_service_member' ORDER BY 1";

#[tokio::test]
async fn upgrade_keeps_other_rules_edges_mappings_and_audit() {
    let Some(db) = v02x("upgrade_0041_replay", true).await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(40, pool).await.expect("migrations up to 0040");
    pool.execute(BEFORE).await.expect("data before the upgrade");
    pool.execute(MORE).await.expect("more data before the upgrade");
    let (rules, edges, classes) = (rows(pool, RULES).await, rows(pool, EDGES).await, rows(pool, CLASSES).await);
    let (mappings, audit, member_rules) =
        (rows(pool, MAPPINGS).await, rows(pool, AUDIT).await, rows(pool, MEMBER_RULE_IDS).await);
    assert_eq!(rules.len(), 4, "{rules:?}");
    assert_eq!(member_rules.len(), 2, "{member_rules:?}");
    let rule_audit = "SELECT entity_id::text FROM audit_log WHERE entity_type = 'relationship_type_rules' ORDER BY 1";
    assert_eq!(rows(pool, rule_audit).await.len(), 4, "one create row per rule before the upgrade");
    MIGRATOR.run_to(41, pool).await.expect("migration 0041");

    let kept: Vec<String> = rules.into_iter().filter(|r| r.starts_with("depends_on:")).collect();
    assert_eq!(kept.len(), 2, "{kept:?}");
    assert_eq!(rows(pool, RULES).await, kept, "only the member type's rules are removed");
    assert_eq!(rows(pool, EDGES).await, edges, "members and direct relationships are kept");
    assert_eq!(rows(pool, CLASSES).await, classes);
    // A saved mapping naming the member type is kept; the API refuses it when
    // it is saved or run (GH#516).
    assert_eq!(rows(pool, MAPPINGS).await, mappings, "saved import mappings are kept");

    // Audit rows from before the upgrade are unchanged; one system `delete`
    // per removed rule follows them, and the chain verifies.
    let after = rows(pool, AUDIT).await;
    assert_eq!(after[..audit.len()], audit[..], "audit rows from before the upgrade are unchanged");
    let deleted = rows(
        pool,
        "SELECT entity_id::text FROM audit_log WHERE actor_type = 'system' AND actor_name = 'migration 0041'
           AND action = 'delete' AND entity_type = 'relationship_type_rules' ORDER BY 1",
    )
    .await;
    assert_eq!(deleted, member_rules, "one delete row per removed rule");
    assert_eq!(after.len(), audit.len() + member_rules.len(), "no other audit rows");
    let problems = rows(pool, "SELECT chain_seq || ' ' || problem || ': ' || detail FROM audit_log_verify()").await;
    assert_eq!(problems, Vec::<String>::new(), "the audit chain verifies (audit-verify)");

    // Upgrading the rest of the way to current main keeps all of it.
    MIGRATOR.run(pool).await.expect("migrations to current");
    assert_eq!(rows(pool, RULES).await, kept);
    assert_eq!(rows(pool, EDGES).await, edges);
    assert_eq!(rows(pool, MAPPINGS).await, mappings);
    assert_eq!(rows(pool, AUDIT).await[..after.len()], after[..]);
    db.drop().await;
}
