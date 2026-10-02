//! Migration 0041 (GH#410, SHAA-1389) against an install with business
//! services: the rules on the member type are removed and no others, members
//! and services are kept, and afterwards the database refuses a rule on the
//! member type and a CI moving into the business service type.

use sqlx::{Executor, PgPool};

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
    MIGRATOR.run_to(41, pool).await.expect("migration 0041");

    let kept: Vec<String> = rules.into_iter().filter(|r| r.starts_with("depends_on:")).collect();
    assert_eq!(rows(pool, RULES).await, kept, "only the member type's rule is removed");
    assert_eq!(rows(pool, EDGES).await, edges, "members are kept");
    assert_eq!(rows(pool, CLASSES).await, classes);

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
    db.drop().await;
}
