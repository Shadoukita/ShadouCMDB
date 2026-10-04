//! Migrations 0046 and 0047 (workflow engine schema, SHAA-1422): exactly the
//! profiles holding `datamodel.manage` gain `workflows.manage`, the permission
//! constraint accepts exactly `GlobalPermission::ALL` (T8), the audit actions
//! are added NOT VALID and validated by the next migration, every class is an
//! asset class, and the workflow tables enforce what the engine relies on:
//! published versions never change, transitions and instances stay inside
//! their own version, one active instance per definition and CI, and events
//! are append-only (for the API role by privilege as well).

use std::collections::BTreeSet;

use sqlx::{Executor, PgPool};
use uuid::Uuid;

use super::upgrade_0029::{constraint_def, literals};
use crate::auth::permissions::GlobalPermission;
use crate::data::crud::AuditAction;
use crate::db::{MIGRATOR, scratch};

const WORKFLOW_ACTIONS: [AuditAction; 6] = [
    AuditAction::WorkflowPublish,
    AuditAction::WorkflowStart,
    AuditAction::WorkflowCancel,
    AuditAction::WorkflowTransition,
    AuditAction::WorkflowMigrate,
    AuditAction::WorkflowForce,
];

const BEFORE: &str = "
INSERT INTO permission_profiles (id, name) VALUES
  ('00000000-0000-4000-8000-0000000000f1', 'Everything but administrator'),
  ('00000000-0000-4000-8000-0000000000f2', 'Modellers'),
  ('00000000-0000-4000-8000-0000000000f3', 'Auditors'),
  ('00000000-0000-4000-8000-0000000000f4', 'Nothing');
INSERT INTO permission_profile_global_permissions (profile_id, permission)
  SELECT '00000000-0000-4000-8000-0000000000f1', p
  FROM unnest(ARRAY['users.manage', 'profiles.manage', 'datamodel.manage', 'customization.manage',
                    'config.export_import', 'audit.view', 'cis.import', 'views.share']) p;
INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES
  ('00000000-0000-4000-8000-0000000000f2', 'datamodel.manage'),
  ('00000000-0000-4000-8000-0000000000f3', 'audit.view');
INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value) VALUES
  ('system', 'test', 'create', 'permission_profiles', '00000000-0000-4000-8000-0000000000f2', NULL, '{}'),
  ('system', 'test', 'export', 'config', '00000000-0000-0000-0000-000000000000', NULL, '{\"kind\": \"config\"}');
";

/// T8: the database accepts exactly the rights the server knows. Kept in the
/// test of the latest migration that changes the list.
pub(crate) async fn assert_permissions_match(pool: &PgPool) {
    let known: BTreeSet<String> = GlobalPermission::ALL.iter().map(|p| p.as_str().to_owned()).collect();
    let def =
        constraint_def(pool, "permission_profile_global_permissions", "permission_profile_global_permissions_valid")
            .await;
    assert_eq!(literals(&def), known, "{def}");
    let scratch: Uuid = sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ('T8 scratch') RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap();
    for p in GlobalPermission::ALL {
        sqlx::query("INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, $2)")
            .bind(scratch)
            .bind(p.as_str())
            .execute(pool)
            .await
            .unwrap_or_else(|e| panic!("{}: {e}", p.as_str()));
    }
    let bogus =
        sqlx::query("INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'x.y')")
            .bind(scratch)
            .execute(pool)
            .await;
    assert!(bogus.is_err());
    sqlx::query("DELETE FROM permission_profiles WHERE id = $1").bind(scratch).execute(pool).await.unwrap();
}

async fn rights(pool: &PgPool) -> Vec<(String, String)> {
    sqlx::query_as(
        "SELECT p.name, g.permission FROM permission_profile_global_permissions g
         JOIN permission_profiles p ON p.id = g.profile_id ORDER BY p.name, g.permission",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn validated(pool: &PgPool, name: &str) -> bool {
    sqlx::query_scalar(
        "SELECT c.convalidated FROM pg_constraint c WHERE c.conrelid = 'cmdb.audit_log'::regclass AND c.conname = $1",
    )
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Runs `sql`; on failure, the SQLSTATE and the message.
async fn run(pool: &PgPool, sql: &str) -> Result<(), (String, String)> {
    match pool.execute(sqlx::AssertSqlSafe(sql.to_owned())).await {
        Ok(_) => Ok(()),
        Err(sqlx::Error::Database(e)) => Err((e.code().unwrap_or_default().into_owned(), e.message().to_owned())),
        Err(e) => panic!("{sql}: {e}"),
    }
}

/// `sql` is refused with this SQLSTATE.
async fn refused(pool: &PgPool, sql: &str, code: &str) -> String {
    match run(pool, sql).await {
        Ok(()) => panic!("expected {code}, but it ran: {sql}"),
        Err((got, message)) => {
            assert_eq!(got, code, "{sql}: {message}");
            message
        }
    }
}

async fn ok(pool: &PgPool, sql: &str) {
    if let Err((code, message)) = run(pool, sql).await {
        panic!("{sql}: {code} {message}");
    }
}

async fn id(pool: &PgPool, sql: &str) -> Uuid {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_owned()))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

/// The rows of one published workflow on `class`, with an active instance on
/// `ci` and two events: one row in every workflow table (fields and attribute
/// references only with an `attribute`).
pub(crate) struct Fixture {
    pub definition: Uuid,
    pub version: Uuid,
    pub planned: Uuid,
    pub done: Uuid,
    pub finish: Uuid,
    pub instance: Uuid,
}

pub(crate) async fn workflow_fixture(
    pool: &PgPool,
    key: &str,
    class: Uuid,
    ci: Uuid,
    attribute: Option<Uuid>,
) -> Fixture {
    let definition = id(
        pool,
        &format!(
            "INSERT INTO workflow_definitions (key, name, class_id, created_by_name, updated_by_name)
             VALUES ('{key}', 'Lifecycle {key}', '{class}', 'test', 'test') RETURNING id"
        ),
    )
    .await;
    let version = id(
        pool,
        &format!(
            "INSERT INTO workflow_versions (definition_id, version_no, status, layout)
             VALUES ('{definition}', 1, 'draft', '{{\"planned\": {{\"x\": 0, \"y\": 0}}}}') RETURNING id"
        ),
    )
    .await;
    let state = |k: &str, category: &str, terminal: bool| {
        format!(
            "INSERT INTO workflow_states (version_id, key, name, category, is_terminal)
             VALUES ('{version}', '{k}', '{k}', '{category}', {terminal}) RETURNING id"
        )
    };
    let planned = id(pool, &state("planned", "open", false)).await;
    let done = id(pool, &state("done", "done", true)).await;
    let finish = id(
        pool,
        &format!(
            "INSERT INTO workflow_transitions (version_id, key, name, from_state_id, to_state_id, requires_comment, conditions)
             VALUES ('{version}', 'finish', 'Finish', '{planned}', '{done}', true, '{{\"all\": []}}') RETURNING id"
        ),
    )
    .await;
    if let Some(attribute) = attribute {
        ok(
            pool,
            &format!(
                "INSERT INTO workflow_transition_fields (transition_id, attribute_id) VALUES ('{finish}', '{attribute}');
                 INSERT INTO workflow_version_attribute_refs (version_id, attribute_id) VALUES ('{version}', '{attribute}')"
            ),
        )
        .await;
    }
    ok(
        pool,
        &format!(
            "UPDATE workflow_versions SET initial_state_id = '{planned}' WHERE id = '{version}';
             UPDATE workflow_versions SET status = 'published', published_at = now(), published_by_name = 'test',
               checksum = sha256('graph'), change_note = 'first' WHERE id = '{version}';
             UPDATE workflow_definitions SET current_version_id = '{version}' WHERE id = '{definition}';
             INSERT INTO workflow_transition_grants (definition_id, transition_key, profile_id)
               SELECT '{definition}', k, id FROM permission_profiles, unnest(ARRAY['finish', '_cancel']) k
               WHERE is_builtin"
        ),
    )
    .await;
    let instance = id(
        pool,
        &format!(
            "INSERT INTO workflow_instances (definition_id, version_id, ci_id, current_state_id, status, started_by_name)
             VALUES ('{definition}', '{version}', '{ci}', '{planned}', 'active', 'test') RETURNING id"
        ),
    )
    .await;
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_instance_events (instance_id, kind, to_state_key, to_version_no, actor_type, actor_name)
               VALUES ('{instance}', 'start', 'planned', 1, 'system', 'test');
             INSERT INTO workflow_instance_events
               (instance_id, kind, transition_key, from_state_key, to_state_key, to_version_no, actor_type, actor_name,
                comment, field_changes, request_id)
               VALUES ('{instance}', 'transition', 'finish', 'planned', 'done', 1, 'user', 'test', 'CAB approved',
                       '{{\"owner\": {{\"old\": null, \"new\": \"x\"}}}}', 'req-1')"
        ),
    )
    .await;
    Fixture { definition, version, planned, done, finish, instance }
}

#[tokio::test]
async fn modellers_gain_workflows_manage_and_the_audit_actions_are_validated_apart() {
    let Some(db) = scratch::empty("modellers_gain_workflows_manage").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(45, pool).await.expect("migrations up to 0045");
    pool.execute(BEFORE).await.expect("data before the upgrade");
    let before = rights(pool).await;
    MIGRATOR.run_to(46, pool).await.expect("migration 0046");

    let after = rights(pool).await;
    let gained: Vec<&(String, String)> = after.iter().filter(|r| !before.contains(r)).collect();
    assert_eq!(
        gained,
        [
            &("Everything but administrator".to_owned(), "workflows.manage".to_owned()),
            &("Modellers".to_owned(), "workflows.manage".to_owned()),
        ],
        "exactly the profiles with datamodel.manage gain workflows.manage"
    );
    assert!(before.iter().all(|r| after.contains(r)), "every existing grant is kept");

    // 0046 re-adds the audit constraints without scanning the log; 0047 validates them.
    for c in ["audit_log_action_valid", "audit_log_values_present"] {
        assert!(!validated(pool, c).await, "{c} is NOT VALID after 0046");
    }
    MIGRATOR.run_to(47, pool).await.expect("migration 0047");
    for c in ["audit_log_action_valid", "audit_log_values_present"] {
        assert!(validated(pool, c).await, "{c} is validated by 0047");
    }
    assert_permissions_match(pool).await;

    let actions = literals(&constraint_def(pool, "audit_log", "audit_log_action_valid").await);
    for a in WORKFLOW_ACTIONS {
        assert!(actions.contains(a.as_str()), "{}", a.as_str());
    }
    for kept in ["create", "export", "import.commit", "schema_change.refused"] {
        assert!(actions.contains(kept), "{kept}");
    }
    // Events carry new_value only; steps that change a CI's state carry both values.
    let audit = |action: &str, old: &str| {
        format!(
            "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value)
             VALUES ('user', 'alice', '{action}', 'configuration_items', gen_random_uuid(), {old}, '{{}}')"
        )
    };
    for event in ["workflow.publish", "workflow.start", "workflow.cancel"] {
        ok(pool, &audit(event, "NULL")).await;
        refused(pool, &audit(event, "'{}'"), "23514").await;
    }
    for change in ["workflow.transition", "workflow.migrate", "workflow.force"] {
        ok(pool, &audit(change, "'{}'")).await;
        refused(pool, &audit(change, "NULL"), "23514").await;
    }
    refused(pool, &audit("workflow.approve", "NULL"), "23514").await;
    let prune: String = sqlx::query_scalar("SELECT prosrc FROM pg_proc WHERE proname = 'prune_audit_log'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert!(WORKFLOW_ACTIONS.iter().all(|a| prune.contains(a.as_str())), "the workflow actions are in a prune scope");

    // Every class is an asset class; the kinds are a closed list; the service type stays an asset type.
    let kinds: Vec<String> = sqlx::query_scalar("SELECT DISTINCT kind FROM ci_classes").fetch_all(pool).await.unwrap();
    assert_eq!(kinds, ["asset"]);
    refused(pool, "UPDATE ci_classes SET kind = 'gadget'", "23514").await;
    refused(pool, "UPDATE ci_classes SET kind = 'process' WHERE system_role = 'business_service'", "23514").await;

    // Running the migrations again is a no-op.
    MIGRATOR.run(pool).await.expect("re-run");
    assert_eq!(rights(pool).await, after);
    db.drop().await;
}

#[tokio::test]
async fn published_versions_are_immutable_and_events_append_only() {
    let Some(db) = scratch::database("workflow_tables_enforce_their_rules").await else { return };
    let pool = &db.pool;
    let class = id(pool, "SELECT id FROM ci_classes WHERE system_role = 'business_service'").await;
    let ci = |label: &str| {
        format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', '{label}') RETURNING id")
    };
    let ci1 = id(pool, &ci("one")).await;
    let ci2 = id(pool, &ci("two")).await;
    let attribute = id(
        pool,
        &format!(
            "INSERT INTO ci_attribute_definitions (class_id, key, label, data_type)
             VALUES ('{class}', 'approver', 'Approver', 'text') RETURNING id"
        ),
    )
    .await;
    let f = workflow_fixture(pool, "lifecycle", class, ci1, Some(attribute)).await;
    let (def, v1, planned, done, finish, instance) = (f.definition, f.version, f.planned, f.done, f.finish, f.instance);
    const IMMUTABLE: &str = "55000";

    // A published version's graph rows never change.
    let msg = refused(pool, &format!("UPDATE workflow_states SET name = 'x' WHERE id = '{planned}'"), IMMUTABLE).await;
    assert!(msg.contains("only a draft"), "{msg}");
    refused(
        pool,
        &format!("INSERT INTO workflow_states (version_id, key, name, category) VALUES ('{v1}', 'extra', 'x', 'open')"),
        IMMUTABLE,
    )
    .await;
    refused(pool, &format!("DELETE FROM workflow_transitions WHERE id = '{finish}'"), IMMUTABLE).await;
    refused(pool, &format!("UPDATE workflow_transitions SET conditions = NULL WHERE id = '{finish}'"), IMMUTABLE).await;
    refused(
        pool,
        &format!("UPDATE workflow_transition_fields SET is_required = false WHERE transition_id = '{finish}'"),
        IMMUTABLE,
    )
    .await;
    refused(pool, &format!("DELETE FROM workflow_version_attribute_refs WHERE version_id = '{v1}'"), IMMUTABLE).await;
    refused(pool, "TRUNCATE workflow_states CASCADE", "42501").await;
    // ... and nor does the version row, except to be retired.
    for set in [
        "layout = '{}'",
        "change_note = 'edited'",
        "checksum = sha256('other')",
        "status = 'draft', published_at = NULL",
    ] {
        refused(pool, &format!("UPDATE workflow_versions SET {set} WHERE id = '{v1}'"), IMMUTABLE).await;
    }
    refused(pool, &format!("UPDATE workflow_versions SET initial_state_id = '{done}' WHERE id = '{v1}'"), IMMUTABLE)
        .await;
    refused(pool, &format!("DELETE FROM workflow_versions WHERE id = '{v1}'"), IMMUTABLE).await;
    refused(
        pool,
        &format!("INSERT INTO workflow_versions (definition_id, version_no, status, published_at) VALUES ('{def}', 9, 'published', now())"),
        IMMUTABLE,
    )
    .await;

    // A draft is edited freely; its graph stays inside the version (composite foreign keys).
    let v2 = id(pool, &format!("INSERT INTO workflow_versions (definition_id, version_no, status) VALUES ('{def}', 2, 'draft') RETURNING id")).await;
    refused(
        pool,
        &format!("INSERT INTO workflow_versions (definition_id, version_no, status) VALUES ('{def}', 3, 'draft')"),
        "23505",
    )
    .await;
    let open2 = id(pool, &format!("INSERT INTO workflow_states (version_id, key, name, category) VALUES ('{v2}', 'planned', 'Planned', 'open') RETURNING id")).await;
    ok(pool, &format!("UPDATE workflow_states SET name = 'Planned (v2)' WHERE id = '{open2}'")).await;
    refused(
        pool,
        &format!(
            "INSERT INTO workflow_transitions (version_id, key, name, from_state_id, to_state_id)
             VALUES ('{v2}', 'finish', 'Finish', '{open2}', '{done}')"
        ),
        "23503",
    )
    .await;
    refused(pool, &format!("UPDATE workflow_versions SET initial_state_id = '{planned}' WHERE id = '{v2}'"), "23503")
        .await;
    refused(
        pool,
        &format!(
            "INSERT INTO workflow_transitions (version_id, key, name, from_state_id, to_state_id)
             VALUES ('{v2}', 'stay', 'Stay', '{open2}', '{open2}')"
        ),
        "23514",
    )
    .await;

    // Instances: their state belongs to their version, their version to their definition.
    let other = workflow_fixture(pool, "other", class, ci2, None).await;
    let instance_sql = |d: Uuid, v: Uuid, c: Uuid, s: Uuid, status: &str| {
        let ended = if status == "active" { "NULL" } else { "now()" };
        format!(
            "INSERT INTO workflow_instances (definition_id, version_id, ci_id, current_state_id, status, ended_at, started_by_name)
             VALUES ('{d}', '{v}', '{c}', '{s}', '{status}', {ended}, 'test')"
        )
    };
    refused(pool, &instance_sql(def, v1, ci2, open2, "active"), "23503").await;
    refused(pool, &instance_sql(def, other.version, ci2, other.planned, "active"), "23503").await;
    refused(
        pool,
        &format!("UPDATE workflow_instances SET current_state_id = '{open2}' WHERE id = '{instance}'"),
        "23503",
    )
    .await;
    refused(
        pool,
        &format!("UPDATE workflow_definitions SET current_version_id = '{}' WHERE id = '{def}'", other.version),
        "23503",
    )
    .await;
    // One active instance per definition and CI; finished ones do not count.
    let msg = refused(pool, &instance_sql(def, v1, ci1, planned, "active"), "23505").await;
    assert!(msg.contains("workflow_instances_one_active"), "{msg}");
    ok(pool, &instance_sql(def, v1, ci1, done, "completed")).await;
    ok(pool, &instance_sql(def, v1, ci2, planned, "active")).await;
    refused(pool, &instance_sql(def, v1, ci1, done, "completed").replace("now()", "NULL"), "23514").await;

    // Events are append-only for everyone, the owner included.
    for sql in [
        format!("UPDATE workflow_instance_events SET comment = 'edited' WHERE instance_id = '{instance}'"),
        format!("DELETE FROM workflow_instance_events WHERE instance_id = '{instance}'"),
        "TRUNCATE workflow_instance_events".to_owned(),
    ] {
        let msg = refused(pool, &sql, "42501").await;
        assert!(msg.contains("append-only"), "{msg}");
    }
    refused(
        pool,
        &format!(
            "INSERT INTO workflow_instance_events (instance_id, kind, to_state_key, to_version_no, actor_type)
             VALUES ('{instance}', 'transition', 'done', 1, 'user')"
        ),
        "23514",
    )
    .await;
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM workflow_instance_events").fetch_one(pool).await.unwrap();
    assert_eq!(events, 4);
    // Neither is deleted while their CI or definition exists. Deleting the CI
    // row itself moves them to the archive since 0050 (workflows::s6_tests).
    refused(pool, &format!("DELETE FROM workflow_instances WHERE id = '{instance}'"), "23503").await;
    refused(pool, &format!("DELETE FROM workflow_definitions WHERE id = '{def}'"), "23503").await;

    // A definition's key and class never change; its settings do.
    refused(pool, &format!("UPDATE workflow_definitions SET key = 'renamed' WHERE id = '{def}'"), IMMUTABLE).await;
    refused(
        pool,
        &format!("UPDATE workflow_definitions SET class_id = gen_random_uuid() WHERE id = '{def}'"),
        IMMUTABLE,
    )
    .await;
    ok(pool, &format!("UPDATE workflow_definitions SET name = 'Renamed', is_active = false WHERE id = '{def}'")).await;
    // Keys are lowercase identifiers, unique.
    let key = |k: &str| {
        format!(
            "INSERT INTO workflow_definitions (key, name, class_id, created_by_name, updated_by_name)
             VALUES ('{k}', 'Clash', '{class}', 't', 't')"
        )
    };
    refused(pool, &key("Lifecycle"), "23514").await;
    refused(pool, &key("lifecycle"), "23505").await;

    // Retiring changes the status only, once.
    ok(pool, &format!("UPDATE workflow_versions SET status = 'retired' WHERE id = '{v1}'")).await;
    refused(pool, &format!("UPDATE workflow_versions SET status = 'published' WHERE id = '{v1}'"), IMMUTABLE).await;
    refused(pool, &format!("UPDATE workflow_states SET name = 'x' WHERE id = '{planned}'"), IMMUTABLE).await;

    // A draft and its graph can be deleted; a definition with its published versions goes in one cascade.
    ok(pool, &format!("DELETE FROM workflow_versions WHERE id = '{v2}'")).await;
    let unused = id(
        pool,
        &format!(
            "INSERT INTO workflow_definitions (key, name, class_id, created_by_name, updated_by_name)
             VALUES ('unused', 'Unused', '{class}', 't', 't') RETURNING id"
        ),
    )
    .await;
    let uv = id(pool, &format!("INSERT INTO workflow_versions (definition_id, version_no, status) VALUES ('{unused}', 1, 'draft') RETURNING id")).await;
    let us = id(pool, &format!("INSERT INTO workflow_states (version_id, key, name, category, is_terminal) VALUES ('{uv}', 's', 'S', 'open', false) RETURNING id")).await;
    let ue = id(pool, &format!("INSERT INTO workflow_states (version_id, key, name, category, is_terminal) VALUES ('{uv}', 'e', 'E', 'done', true) RETURNING id")).await;
    ok(
        pool,
        &format!(
            "INSERT INTO workflow_transitions (version_id, key, name, from_state_id, to_state_id) VALUES ('{uv}', 'go', 'Go', '{us}', '{ue}');
             INSERT INTO workflow_version_attribute_refs (version_id, attribute_id) VALUES ('{uv}', '{attribute}');
             UPDATE workflow_versions SET initial_state_id = '{us}', status = 'published', published_at = now(),
               published_by_name = 't', checksum = sha256('g') WHERE id = '{uv}';
             UPDATE workflow_definitions SET current_version_id = '{uv}' WHERE id = '{unused}'"
        ),
    )
    .await;
    ok(pool, &format!("DELETE FROM workflow_definitions WHERE id = '{unused}'")).await;
    let left: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT (SELECT count(*) FROM workflow_versions WHERE definition_id = '{unused}')
              + (SELECT count(*) FROM workflow_states WHERE version_id = '{uv}')
              + (SELECT count(*) FROM workflow_transitions WHERE version_id = '{uv}')
              + (SELECT count(*) FROM workflow_version_attribute_refs WHERE version_id = '{uv}')"
    )))
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(left, 0, "the cascade removed the published version and its graph");
    // The attribute a version references cannot be deleted under it.
    refused(pool, &format!("DELETE FROM ci_attribute_definitions WHERE id = '{attribute}'"), "23503").await;
    db.drop().await;
}

/// On a three-role install the API role may read and append events but not
/// change or remove them; the other workflow tables are ordinary DML tables.
#[tokio::test]
async fn the_api_role_only_appends_workflow_events() {
    let Some(roles) = scratch::Roles::create("the_api_role_only_appends_workflow_events").await else { return };
    let db = roles.database().await;
    let owner = &db.pool;
    let privilege = |table: &str, privilege: &str| {
        let (role, table, privilege) = (roles.app.clone(), table.to_owned(), privilege.to_owned());
        async move {
            sqlx::query_scalar::<_, bool>("SELECT has_table_privilege($1, $2, $3)")
                .bind(role)
                .bind(format!("cmdb.{table}"))
                .bind(privilege)
                .fetch_one(owner)
                .await
                .unwrap()
        }
    };
    for p in ["SELECT", "INSERT"] {
        assert!(privilege("workflow_instance_events", p).await, "{p}");
    }
    for p in ["UPDATE", "DELETE", "TRUNCATE"] {
        assert!(!privilege("workflow_instance_events", p).await, "{p}");
    }
    for t in [
        "workflow_definitions",
        "workflow_versions",
        "workflow_states",
        "workflow_transitions",
        "workflow_transition_fields",
        "workflow_version_attribute_refs",
        "workflow_transition_grants",
        "workflow_instances",
    ] {
        for p in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            assert!(privilege(t, p).await, "{t} {p}");
        }
    }

    let class = id(owner, "SELECT id FROM ci_classes WHERE system_role = 'business_service'").await;
    let ci =
        id(owner, &format!("INSERT INTO configuration_items (class_id, label) VALUES ('{class}', 'one') RETURNING id"))
            .await;
    let f = workflow_fixture(owner, "lifecycle", class, ci, None).await;
    let api = roles.api_pool(&db).await;
    let current: String = sqlx::query_scalar("SELECT current_user::text").fetch_one(&api).await.unwrap();
    assert_eq!(current, roles.app);
    ok(
        &api,
        &format!(
            "INSERT INTO cmdb.workflow_instance_events (instance_id, kind, to_state_key, to_version_no, actor_type)
             VALUES ('{}', 'cancel', 'planned', 1, 'user')",
            f.instance
        ),
    )
    .await;
    refused(
        &api,
        &format!("UPDATE cmdb.workflow_instance_events SET comment = 'x' WHERE instance_id = '{}'", f.instance),
        "42501",
    )
    .await;
    refused(&api, &format!("DELETE FROM cmdb.workflow_instance_events WHERE instance_id = '{}'", f.instance), "42501")
        .await;
    ok(&api, &format!("UPDATE cmdb.workflow_instances SET version = version + 1 WHERE id = '{}'", f.instance)).await;
    api.close().await;
    db.drop().await;
    roles.drop().await;
}
