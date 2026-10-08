//! Migrations 0033 and 0034 (business services data model, SHAA-929, spec
//! SHAA-927 §6.1, §6.2, §7.1): the service class is adopted or created, the
//! member type exists, the protections and the membership check fire on
//! direct SQL, concurrent inserts cannot jointly close a loop, and the owner
//! and group tables hold their constraints.

use std::time::Duration;

use sqlx::{Connection, Executor, PgPool};
use uuid::Uuid;

use crate::api::context::RequestContext;
use crate::db::{MIGRATOR, scratch};

/// The constraint a failed statement names, or panics when it succeeded.
fn constraint<T: std::fmt::Debug>(res: Result<T, sqlx::Error>) -> String {
    let err = res.expect_err("the statement should have been rejected");
    let db = err.as_database_error().unwrap_or_else(|| panic!("{err}"));
    db.constraint().unwrap_or_else(|| panic!("no constraint: {db}")).to_owned()
}

async fn id(pool: &PgPool, sql: &str) -> Uuid {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

async fn reconcile(pool: &PgPool) {
    let ctx = RequestContext::system("test", "test");
    let mut tx = pool.begin().await.unwrap();
    crate::schema::reconcile(&mut tx, &ctx, "test").await.unwrap_or_else(|e| panic!("reconcile: {}", e.message));
    tx.commit().await.unwrap();
}

/// A v0.2.x data model (migration 0032) with a "service" class: the starter
/// template's when `tier` is set, else a customised one without service_tier.
const V02X: &str = "
INSERT INTO areas (id, key, name) VALUES ('00000000-0000-4000-8000-0000000000a1', 'infrastruktur', 'Infrastruktur');
INSERT INTO ci_classes (id, key, name, area_id, color) VALUES
  ('00000000-0000-4000-8000-0000000000c1', 'application', 'Application', '00000000-0000-4000-8000-0000000000a1', '#0969da'),
  ('00000000-0000-4000-8000-0000000000c2', 'service', 'Service', '00000000-0000-4000-8000-0000000000a1', '#cf222e');
INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, is_required, sort_order)
  SELECT id, 'name', 'Name', 'text', true, 0 FROM ci_classes;
INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, sort_order) VALUES
  ('00000000-0000-4000-8000-0000000000c2', 'sla_uptime_percent', 'SLA uptime (%)', 'number', 20);
UPDATE ci_classes c SET title_attribute_id = d.id FROM ci_attribute_definitions d WHERE d.class_id = c.id AND d.key = 'name';
INSERT INTO relationship_types (id, key, name, forward_label, reverse_label, impact_direction) VALUES
  ('00000000-0000-4000-8000-0000000000d1', 'depends_on', 'Depends on', 'depends on', 'is required by', 'target_to_source');
INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id) VALUES
  ('00000000-0000-4000-8000-0000000000d1', '00000000-0000-4000-8000-0000000000c2', '00000000-0000-4000-8000-0000000000c1');
INSERT INTO permission_profiles (id, name) VALUES ('00000000-0000-4000-8000-0000000000f1', 'Service desk');
INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view, can_edit)
  VALUES ('00000000-0000-4000-8000-0000000000f1', '00000000-0000-4000-8000-0000000000c2', true, true);
";

const TIER: &str = "
INSERT INTO ci_attribute_definitions (class_id, key, label, data_type, enum_values, sort_order) VALUES
  ('00000000-0000-4000-8000-0000000000c2', 'service_tier', 'Service tier', 'enum', '[\"tier_1\", \"tier_2\", \"tier_3\"]', 10);
";

const V02X_CIS: &str = "
INSERT INTO configuration_items (id, class_id, label) VALUES
  ('00000000-0000-4000-8000-000000000101', '00000000-0000-4000-8000-0000000000c1', 'Shop app'),
  ('00000000-0000-4000-8000-000000000201', '00000000-0000-4000-8000-0000000000c2', 'Online shop'),
  ('00000000-0000-4000-8000-000000000202', '00000000-0000-4000-8000-0000000000c2', 'Payroll');
INSERT INTO infrastruktur.application (id, name) VALUES ('00000000-0000-4000-8000-000000000101', 'Shop app');
INSERT INTO infrastruktur.service (id, name, sla_uptime_percent) VALUES
  ('00000000-0000-4000-8000-000000000201', 'Online shop', 99.9),
  ('00000000-0000-4000-8000-000000000202', 'Payroll', 99.5);
INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id) VALUES
  ('00000000-0000-4000-8000-0000000000d1', '00000000-0000-4000-8000-000000000201', '00000000-0000-4000-8000-000000000101');
";

const SERVICE: &str = "00000000-0000-4000-8000-0000000000c2";

/// Row hashes of everything adoption must leave alone. The service class row
/// is compared without system_role, the one column adoption sets.
async fn fingerprint(pool: &PgPool) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for (name, sql) in [
        ("configuration_items", "SELECT md5(string_agg(t::text, '|' ORDER BY t.id)) FROM configuration_items t"),
        ("service table", "SELECT md5(string_agg(t::text, '|' ORDER BY t.id)) FROM infrastruktur.service t"),
        ("application table", "SELECT md5(string_agg(t::text, '|' ORDER BY t.id)) FROM infrastruktur.application t"),
        // Without what 0044 adds (the Person type, its fields and their system_role
        // column) and the is_expected column 0067 adds.
        (
            "attributes",
            "SELECT md5(string_agg((to_jsonb(t) - 'system_role' - 'is_expected')::text, '|' ORDER BY t.id)) FROM ci_attribute_definitions t
             WHERE t.class_id NOT IN (SELECT c.id FROM ci_classes c WHERE to_jsonb(c) ->> 'system_role' = 'person')",
        ),
        ("relationships", "SELECT md5(string_agg(t::text, '|' ORDER BY t.id)) FROM ci_relationships t"),
        ("rules", "SELECT md5(string_agg(t::text, '|' ORDER BY t.id)) FROM relationship_type_rules t"),
        ("grants", "SELECT md5(string_agg(t::text, '|' ORDER BY t.id)) FROM permission_profile_class_permissions t"),
        // Without the kind column 0046 adds and the data-quality fields 0068 adds.
        (
            "classes",
            "SELECT md5(string_agg((to_jsonb(t) - 'system_role' - 'kind' - 'owner_attribute_id' - 'end_of_life_attribute_id')::text, '|' ORDER BY t.id)) FROM ci_classes t
             WHERE (to_jsonb(t) ->> 'system_role') IS DISTINCT FROM 'person'",
        ),
    ] {
        let hash: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql)).fetch_one(pool).await.unwrap();
        out.push((name.to_owned(), hash));
    }
    out
}

pub(crate) async fn v02x(test: &str, tier: bool) -> Option<scratch::Scratch> {
    let db = scratch::empty(test).await?;
    MIGRATOR.run_to(32, &db.pool).await.expect("migrations up to 0032");
    db.pool.execute(V02X).await.expect("v0.2.x data model");
    if tier {
        db.pool.execute(TIER).await.expect("service_tier");
    }
    reconcile(&db.pool).await;
    db.pool.execute(V02X_CIS).await.expect("v0.2.x CIs");
    Some(db)
}

async fn system_class(pool: &PgPool) -> (Uuid, String, String) {
    sqlx::query_as(
        "SELECT c.id, c.key, a.key FROM ci_classes c JOIN areas a ON a.id = c.area_id WHERE c.system_role = 'business_service'",
    )
    .fetch_one(pool)
    .await
    .expect("exactly one business service class")
}

async fn member_type(pool: &PgPool) -> Uuid {
    id(pool, "SELECT id FROM relationship_types WHERE system_role = 'business_service_member'").await
}

#[tokio::test]
async fn upgrade_adopts_the_template_service_class_and_changes_no_data() {
    let Some(db) = v02x("upgrade_adopts_the_template_service_class", true).await else { return };
    let pool = &db.pool;
    let before = fingerprint(pool).await;
    let areas_before: i64 = sqlx::query_scalar("SELECT count(*) FROM areas").fetch_one(pool).await.unwrap();
    MIGRATOR.run(pool).await.expect("migrations 0033 and 0034");

    let (class, key, area) = system_class(pool).await;
    assert_eq!((class.to_string().as_str(), key.as_str(), area.as_str()), (SERVICE, "service", "infrastruktur"));
    assert_eq!(fingerprint(pool).await, before, "adoption changes no data");
    // Only the Person type's area (0044) is new.
    let areas_after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM areas WHERE key NOT LIKE 'people%'").fetch_one(pool).await.unwrap();
    assert_eq!(areas_after, areas_before, "no new area");
    let business_service: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_classes WHERE key LIKE 'business_service%'")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(business_service, 0, "no second service class");

    // The adopted class's CIs can include members straight away.
    let member = member_type(pool).await;
    sqlx::query(
        "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id)
         VALUES ($1, '00000000-0000-4000-8000-000000000201', '00000000-0000-4000-8000-000000000101'),
                ($1, '00000000-0000-4000-8000-000000000201', '00000000-0000-4000-8000-000000000202')",
    )
    .bind(member)
    .execute(pool)
    .await
    .expect("members of the adopted class");
    db.drop().await;
}

#[tokio::test]
async fn upgrade_without_service_tier_creates_a_new_class() {
    let Some(db) = v02x("upgrade_without_service_tier_creates_a_new_class", false).await else { return };
    let pool = &db.pool;
    let before = fingerprint(pool).await;
    MIGRATOR.run(pool).await.expect("migrations 0033 and 0034");

    let (class, key, area) = system_class(pool).await;
    assert_ne!(class.to_string(), SERVICE);
    assert_eq!((key.as_str(), area.as_str()), ("business_service", "business_services"));
    let before_without_new: Vec<_> = before.iter().filter(|(n, _)| n != "classes" && n != "attributes").collect();
    let after = fingerprint(pool).await;
    let after_without_new: Vec<_> = after.iter().filter(|(n, _)| n != "classes" && n != "attributes").collect();
    assert_eq!(after_without_new, before_without_new, "existing data unchanged");
    let customised: Option<String> = sqlx::query_scalar("SELECT system_role FROM ci_classes WHERE id = $1::uuid")
        .bind(SERVICE)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(customised, None, "the customised service class is not adopted");

    // The new class: "Business service", the template's colour, a required name as its title.
    let (name, color, title): (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT c.name, c.color, d.key FROM ci_classes c LEFT JOIN ci_attribute_definitions d ON d.id = c.title_attribute_id
         WHERE c.id = $1",
    )
    .bind(class)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        (name.as_str(), color.as_deref(), title.as_deref()),
        ("Business service", Some("#cf222e"), Some("name"))
    );
    // Profiles with explicit grants do not see it until an administrator grants it.
    let grants: i64 =
        sqlx::query_scalar("SELECT count(*) FROM permission_profile_class_permissions WHERE class_id = $1")
            .bind(class)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(grants, 0);

    // `shadoucmdb migrate` then builds its schema and table.
    reconcile(pool).await;
    let ci =
        id(pool, &format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', 'Shop') RETURNING id"))
            .await;
    sqlx::query("INSERT INTO business_services.business_service (id, name) VALUES ($1, 'Shop')")
        .bind(ci)
        .execute(pool)
        .await
        .expect("the new type's table");
    db.drop().await;
}

#[tokio::test]
async fn taken_keys_get_a_suffix() {
    let Some(db) = scratch::empty("taken_keys_get_a_suffix").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(32, pool).await.expect("migrations up to 0032");
    pool.execute(
        "INSERT INTO areas (id, key, name) VALUES ('00000000-0000-4000-8000-0000000000a1', 'business_services', 'Mine');
         INSERT INTO ci_classes (key, name, area_id) VALUES
           ('business_service', 'Mine', '00000000-0000-4000-8000-0000000000a1');
         INSERT INTO relationship_types (key, name, forward_label, reverse_label)
           VALUES ('business_service_member', 'Mine', 'a', 'b');",
    )
    .await
    .unwrap();
    MIGRATOR.run(pool).await.expect("migrations 0033 and 0034");

    let (_, key, area) = system_class(pool).await;
    assert_eq!((key.as_str(), area.as_str()), ("business_service_2", "business_services_2"));
    let (key, name, forward, reverse, directional, impact, active): (
        String,
        String,
        String,
        String,
        bool,
        String,
        bool,
    ) = sqlx::query_as(
        "SELECT key, name, forward_label, reverse_label, is_directional, impact_direction, is_active
             FROM relationship_types WHERE system_role = 'business_service_member'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        (key.as_str(), name.as_str(), forward.as_str(), reverse.as_str(), directional, impact.as_str(), active),
        ("business_service_member_2", "Service member", "includes", "is part of", true, "target_to_source", true)
    );
    db.drop().await;
}

/// A new installation gets the class; installing the starter template then
/// puts the template's service fields and rules on it instead of adding a
/// second service class.
#[tokio::test]
async fn fresh_install_creates_the_class_and_the_template_adopts_it() {
    let Some(db) = scratch::database("fresh_install_creates_the_class").await else { return };
    let pool = &db.pool;
    let (class, key, area) = system_class(pool).await;
    assert_eq!((key.as_str(), area.as_str()), ("business_service", "business_services"));
    member_type(pool).await;
    reconcile(pool).await;

    // The built-in class alone is no partial install: the templates page still says the CMDB is empty.
    use crate::modules::templates::TemplateStatus;
    let status = |list: crate::modules::templates::StarterTemplateList| {
        let t = list.data.into_iter().find(|t| t.key == "it_infrastructure").unwrap();
        (t.status, t.present.total())
    };
    let listed = crate::modules::templates::list(pool).await.unwrap_or_else(|e| panic!("{}", e.message));
    assert_eq!(status(listed), (TemplateStatus::NotInstalled, 0));

    let template = crate::modules::templates::find("it_infrastructure").unwrap();
    let ctx = RequestContext::system("test", "test");
    let mut tx = pool.begin().await.unwrap();
    let installed =
        crate::modules::templates::install(&mut tx, &ctx, template).await.unwrap_or_else(|e| panic!("{}", e.message));
    tx.commit().await.unwrap();
    assert_eq!(installed.existing.total(), 0, "nothing of the template was there");
    let listed = crate::modules::templates::list(pool).await.unwrap_or_else(|e| panic!("{}", e.message));
    let (installed_status, _) = status(listed);
    assert_eq!(installed_status, TemplateStatus::Installed);

    let service: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_classes WHERE key = 'service'").fetch_one(pool).await.unwrap();
    assert_eq!(service, 0, "no second service class");
    let fields: Vec<String> =
        sqlx::query_scalar("SELECT key FROM ci_attribute_definitions WHERE class_id = $1 ORDER BY key")
            .bind(class)
            .fetch_all(pool)
            .await
            .unwrap();
    for f in ["name", "service_tier", "sla_uptime_percent", "support_url"] {
        assert!(fields.iter().any(|k| k == f), "{f} in {fields:?}");
    }
    let rules: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM relationship_type_rules r JOIN relationship_types t ON t.id = r.relationship_type_id
         WHERE t.key = 'depends_on' AND r.source_class_id = $1",
    )
    .bind(class)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(rules, 2, "depends_on service -> application and service -> service");

    // Re-installing is still a no-op.
    let mut tx = pool.begin().await.unwrap();
    let again =
        crate::modules::templates::install(&mut tx, &ctx, template).await.unwrap_or_else(|e| panic!("{}", e.message));
    tx.commit().await.unwrap();
    assert_eq!(again.created.classes + again.created.attribute_definitions + again.created.relationship_rules, 0);
    db.drop().await;
}

#[tokio::test]
async fn the_service_class_and_the_member_type_are_protected() {
    let Some(db) = scratch::database("the_service_class_and_the_member_type_are_protected").await else { return };
    let pool = &db.pool;
    let (class, _, _) = system_class(pool).await;
    let area = id(pool, "SELECT area_id FROM ci_classes WHERE system_role = 'business_service'").await;
    let other = id(
        pool,
        &format!("INSERT INTO ci_classes (key, name, area_id) VALUES ('other', 'Other', '{area}') RETURNING id"),
    )
    .await;
    let run = |sql: String| async move { sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await };

    for sql in [
        format!("DELETE FROM ci_classes WHERE id = '{class}'"),
        format!("UPDATE ci_classes SET is_active = false WHERE id = '{class}'"),
        format!("UPDATE ci_classes SET is_abstract = true WHERE id = '{class}'"),
        format!("UPDATE ci_classes SET parent_id = '{other}' WHERE id = '{class}'"),
        format!("UPDATE ci_classes SET system_role = NULL WHERE id = '{class}'"),
        format!("UPDATE ci_classes SET system_role = 'business_service' WHERE id = '{other}'"),
        format!(
            "INSERT INTO ci_classes (key, name, area_id, system_role) VALUES ('x', 'X', '{area}', 'business_service')"
        ),
    ] {
        assert_eq!(constraint(run(sql.clone()).await), "ci_classes_system_protected", "{sql}");
    }
    for sql in [
        format!("INSERT INTO ci_classes (key, name, area_id, parent_id) VALUES ('sub', 'Sub', '{area}', '{class}')"),
        format!("UPDATE ci_classes SET parent_id = '{class}' WHERE id = '{other}'"),
    ] {
        assert_eq!(constraint(run(sql.clone()).await), "ci_classes_system_no_subclass", "{sql}");
    }
    // Name, description, colour and icon stay the administrator's.
    run(format!(
        "UPDATE ci_classes SET name = 'Services', description = 'd', color = '#000000', icon = 'x' WHERE id = '{class}'"
    ))
    .await
    .expect("rename");

    let member = member_type(pool).await;
    assert_eq!(
        constraint(run(format!("DELETE FROM relationship_types WHERE id = '{member}'")).await),
        "relationship_types_system_protected"
    );
    assert_eq!(
        constraint(run(format!("UPDATE relationship_types SET system_role = NULL WHERE id = '{member}'")).await),
        "relationship_types_system_protected"
    );
    assert_eq!(
        constraint(
            run("INSERT INTO relationship_types (key, name, forward_label, reverse_label, system_role)
                 VALUES ('m2', 'M', 'a', 'b', 'business_service_member')"
                .into())
            .await
        ),
        "relationship_types_system_protected"
    );
    for (column, set) in [
        ("key", "key = 'members'"),
        ("is_directional", "is_directional = false, impact_direction = 'both'"),
        ("impact_direction", "impact_direction = 'none'"),
        ("is_active", "is_active = false"),
    ] {
        let err = run(format!("UPDATE relationship_types SET {set} WHERE id = '{member}'")).await.unwrap_err();
        let pg = err.as_database_error().unwrap().try_downcast_ref::<sqlx::postgres::PgDatabaseError>().unwrap();
        assert_eq!((pg.constraint(), pg.column()), (Some("relationship_types_system_fixed"), Some(column)), "{set}");
    }
    run(format!(
        "UPDATE relationship_types SET name = 'Member', forward_label = 'contains', reverse_label = 'is in', description = 'd'
         WHERE id = '{member}'"
    ))
    .await
    .expect("labels");
    db.drop().await;
}

struct Services {
    pool: PgPool,
    class: Uuid,
    member: Uuid,
}

impl Services {
    async fn new(pool: &PgPool) -> Self {
        let (class, _, _) = system_class(pool).await;
        Services { pool: pool.clone(), class, member: member_type(pool).await }
    }

    async fn ci(&self, class: Uuid) -> Uuid {
        id(
            &self.pool,
            &format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', 'x') RETURNING id"),
        )
        .await
    }

    async fn service(&self) -> Uuid {
        self.ci(self.class).await
    }

    async fn include<'e, E: sqlx::PgExecutor<'e>>(
        &self,
        conn: E,
        service: Uuid,
        member: Uuid,
    ) -> Result<Uuid, sqlx::Error> {
        sqlx::query_scalar(
            "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(self.member)
        .bind(service)
        .bind(member)
        .fetch_one(conn)
        .await
    }
}

#[tokio::test]
async fn membership_rules_hold_on_direct_sql() {
    let Some(db) = scratch::database("membership_rules_hold_on_direct_sql").await else { return };
    let pool = &db.pool;
    let s = Services::new(pool).await;
    let area = id(pool, "SELECT area_id FROM ci_classes WHERE system_role = 'business_service'").await;
    let other_class = id(
        pool,
        &format!("INSERT INTO ci_classes (key, name, area_id) VALUES ('server', 'Server', '{area}') RETURNING id"),
    )
    .await;
    let server = s.ci(other_class).await;
    let [a, b, c] = [s.service().await, s.service().await, s.service().await];

    // Any class may be a member, with no relationship rule; only a service includes.
    s.include(pool, a, server).await.expect("a server as member");
    assert_eq!(constraint(s.include(pool, server, a).await), "membership_source");
    assert_eq!(constraint(s.include(pool, a, a).await), "membership_self");
    assert_eq!(constraint(s.include(pool, a, server).await), "ci_relationships_live_edge_uq");

    // A includes B includes C; C including A (or B including A) closes a loop.
    let ab = s.include(pool, a, b).await.unwrap();
    s.include(pool, b, c).await.unwrap();
    assert_eq!(constraint(s.include(pool, c, a).await), "membership_cycle");
    assert_eq!(constraint(s.include(pool, b, a).await), "membership_cycle");

    // Moving an edge is checked like an insert.
    let moved = sqlx::query("UPDATE ci_relationships SET source_ci_id = $1, target_ci_id = $2 WHERE id = $3")
        .bind(c)
        .bind(a)
        .bind(ab)
        .execute(pool)
        .await;
    assert_eq!(constraint(moved), "membership_cycle");

    // Restoring a removed edge that would now close a loop is refused.
    sqlx::query("UPDATE ci_relationships SET deleted_at = now() WHERE id = $1").bind(ab).execute(pool).await.unwrap();
    s.include(pool, c, a).await.expect("no loop while A -> B is removed");
    let restored =
        sqlx::query("UPDATE ci_relationships SET deleted_at = NULL WHERE id = $1").bind(ab).execute(pool).await;
    assert_eq!(constraint(restored), "membership_cycle");

    // A service with members keeps its type.
    let retype = sqlx::query("UPDATE configuration_items SET class_id = $1 WHERE id = $2")
        .bind(other_class)
        .bind(b)
        .execute(pool)
        .await;
    assert_eq!(constraint(retype), "configuration_items_service_members");
    db.drop().await;
}

#[tokio::test]
async fn nesting_depth_is_bounded() {
    let Some(db) = scratch::database("nesting_depth_is_bounded").await else { return };
    let pool = &db.pool;
    let s = Services::new(pool).await;

    // Without a setting the ceiling is 8 levels: 9 services in a chain.
    let mut chain = Vec::new();
    for _ in 0..10 {
        chain.push(s.service().await);
    }
    for w in chain[..9].windows(2) {
        s.include(pool, w[0], w[1]).await.expect("within 8 levels");
    }
    assert_eq!(constraint(s.include(pool, chain[8], chain[9]).await), "membership_nesting_depth");
    // Above the top as well as below the bottom.
    assert_eq!(constraint(s.include(pool, chain[9], chain[0]).await), "membership_nesting_depth");

    // The API passes BUSINESS_SERVICE_MAX_NESTING per transaction. Two chains
    // of 2 levels joined in the middle make 5 levels: allowed at 5, not at 4.
    let [p, q, r, t, u, v] = [
        s.service().await,
        s.service().await,
        s.service().await,
        s.service().await,
        s.service().await,
        s.service().await,
    ];
    for (x, y) in [(p, q), (q, r), (t, u), (u, v)] {
        s.include(pool, x, y).await.unwrap();
    }
    for (limit, ok) in [("4", false), ("5", true)] {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("SELECT set_config('shadoucmdb.business_service_max_nesting', $1, true)")
            .bind(limit)
            .execute(&mut *tx)
            .await
            .unwrap();
        let res = s.include(&mut *tx, r, t).await;
        if ok {
            res.expect("5 levels at a limit of 5");
            tx.commit().await.unwrap();
        } else {
            assert_eq!(constraint(res), "membership_nesting_depth");
        }
    }
    db.drop().await;
}

/// Two sessions each add one half of a loop (A includes B, B includes A).
/// The second waits for the first's lock, then sees its edge: exactly one wins.
#[tokio::test]
async fn concurrent_inserts_cannot_jointly_close_a_loop() {
    let Some(db) = scratch::database("concurrent_inserts_cannot_jointly_close_a_loop").await else { return };
    let pool = &db.pool;
    let s = Services::new(pool).await;
    let (a, b) = (s.service().await, s.service().await);

    let mut first = pool.begin().await.unwrap();
    s.include(&mut *first, a, b).await.expect("A includes B");

    let mut second = pool.acquire().await.unwrap().detach();
    let (member, pid): (Uuid, i32) =
        (s.member, sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut second).await.unwrap());
    let racer = tokio::spawn(async move {
        let res = sqlx::query(
            "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id) VALUES ($1, $2, $3)",
        )
        .bind(member)
        .bind(b)
        .bind(a)
        .execute(&mut second)
        .await;
        second.close().await.ok();
        res
    });
    // Wait until the second session is queued behind the advisory lock.
    let mut waiting = false;
    for _ in 0..200 {
        waiting = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE pid = $1 AND locktype = 'advisory' AND NOT granted)",
        )
        .bind(pid)
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(waiting, "the second insert waits for the membership lock");
    first.commit().await.unwrap();

    assert_eq!(constraint(racer.await.unwrap()), "membership_cycle");
    let edges: i64 = sqlx::query_scalar("SELECT count(*) FROM ci_relationships WHERE relationship_type_id = $1")
        .bind(s.member)
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(edges, 1, "exactly one of the two inserts succeeded");
    db.drop().await;
}

#[tokio::test]
async fn owners_and_groups_keep_their_constraints() {
    let Some(db) = scratch::database("owners_and_groups_keep_their_constraints").await else { return };
    let pool = &db.pool;
    let s = Services::new(pool).await;
    let service = s.service().await;
    let area = id(pool, "SELECT area_id FROM ci_classes WHERE system_role = 'business_service'").await;
    let other_class = id(
        pool,
        &format!("INSERT INTO ci_classes (key, name, area_id) VALUES ('server', 'Server', '{area}') RETURNING id"),
    )
    .await;
    let server = s.ci(other_class).await;
    let user = id(
        pool,
        "INSERT INTO users (username, display_name, password_hash) VALUES ('alice', 'Alice', '$argon2id$v=19$test') RETURNING id",
    )
    .await;
    let group = id(pool, "INSERT INTO user_groups (name) VALUES ('Web team') RETURNING id").await;
    sqlx::query("INSERT INTO user_group_members (group_id, user_id) VALUES ($1, $2)")
        .bind(group)
        .bind(user)
        .execute(pool)
        .await
        .unwrap();

    assert_eq!(
        constraint(sqlx::query("INSERT INTO user_groups (name) VALUES ('WEB TEAM')").execute(pool).await),
        "user_groups_name_uq"
    );
    assert_eq!(
        constraint(sqlx::query("INSERT INTO user_groups (name) VALUES ('  ')").execute(pool).await),
        "user_groups_name_not_blank"
    );

    let owner = |ci: Uuid, role: &'static str, user: Option<Uuid>, group: Option<Uuid>, position: i32| {
        sqlx::query(
            "INSERT INTO business_service_owners (service_ci_id, role, user_id, group_id, position) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(ci)
        .bind(role)
        .bind(user)
        .bind(group)
        .bind(position)
        .execute(pool)
    };
    owner(service, "technical", Some(user), None, 0).await.unwrap();
    owner(service, "technical", None, Some(group), 1).await.unwrap();
    owner(service, "business", Some(user), None, 0).await.unwrap();
    assert_eq!(constraint(owner(service, "technical", Some(user), None, 2).await), "business_service_owners_user_uq");
    assert_eq!(constraint(owner(service, "technical", None, Some(group), 2).await), "business_service_owners_group_uq");
    assert_eq!(
        constraint(owner(service, "technical", Some(user), Some(group), 2).await),
        "business_service_owners_one_principal"
    );
    assert_eq!(constraint(owner(service, "technical", None, None, 2).await), "business_service_owners_one_principal");
    assert_eq!(constraint(owner(service, "sponsor", Some(user), None, 2).await), "business_service_owners_role_valid");
    assert_eq!(constraint(owner(server, "technical", Some(user), None, 0).await), "business_service_owners_service");

    // A service with owners keeps its type.
    let retype = sqlx::query("UPDATE configuration_items SET class_id = $1 WHERE id = $2")
        .bind(other_class)
        .bind(service)
        .execute(pool)
        .await;
    assert_eq!(constraint(retype), "configuration_items_service_members");

    // Deleting the group, then the user, takes their ownerships and memberships with them.
    sqlx::query("DELETE FROM user_groups WHERE id = $1").bind(group).execute(pool).await.unwrap();
    let count = |sql: &'static str| async move { sqlx::query_scalar::<_, i64>(sql).fetch_one(pool).await.unwrap() };
    assert_eq!(count("SELECT count(*) FROM business_service_owners").await, 2);
    assert_eq!(count("SELECT count(*) FROM user_group_members").await, 0);
    sqlx::query("DELETE FROM users WHERE id = $1").bind(user).execute(pool).await.unwrap();
    assert_eq!(count("SELECT count(*) FROM business_service_owners").await, 0);
    db.drop().await;
}

/// The new tables have the §6.1 indexes, and ci_relationships gains none.
#[tokio::test]
async fn indexes_match_the_spec() {
    let Some(db) = scratch::empty("business_services_indexes_match_the_spec").await else { return };
    let pool = &db.pool;
    let indexes = |table: &'static str| async move {
        sqlx::query_scalar::<_, String>(
            "SELECT indexname::text FROM pg_indexes WHERE schemaname = 'cmdb' AND tablename = $1 ORDER BY 1",
        )
        .bind(table)
        .fetch_all(pool)
        .await
        .unwrap()
    };
    MIGRATOR.run_to(32, pool).await.unwrap();
    let relationships = indexes("ci_relationships").await;
    MIGRATOR.run(pool).await.unwrap();
    assert_eq!(indexes("ci_relationships").await, relationships);
    assert_eq!(
        indexes("business_service_owners").await,
        [
            "business_service_owners_group_idx",
            "business_service_owners_group_uq",
            "business_service_owners_pkey",
            "business_service_owners_service_idx",
            "business_service_owners_user_idx",
            "business_service_owners_user_uq"
        ]
    );
    assert_eq!(indexes("user_group_members").await, ["user_group_members_pkey", "user_group_members_user_idx"]);
    assert_eq!(indexes("user_groups").await, ["user_groups_name_uq", "user_groups_pkey"]);
    db.drop().await;
}
