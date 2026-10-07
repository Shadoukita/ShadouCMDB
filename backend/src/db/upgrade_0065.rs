//! Migration 0065 (one list of the API role's rights, GH#713): a single-role
//! install split with sql/bootstrap/10_split_roles.sql grants the API role
//! exactly what a fresh three-role install does, and an install split before
//! this release is repaired by the migration.

use sqlx::PgPool;

use crate::db::{MIGRATOR, scratch};

/// What the API role may do outside the area schemas, one line per privilege.
async fn api_role_privileges(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(include_str!("../../../sql/checks/api_role_privileges.sql")).fetch_all(pool).await.unwrap()
}

/// The lines only one side has, so a failure names the privileges that differ.
fn assert_same(actual: &[String], expected: &[String]) {
    let extra: Vec<_> = actual.iter().filter(|p| !expected.contains(p)).collect();
    let missing: Vec<_> = expected.iter().filter(|p| !actual.contains(p)).collect();
    assert!(
        extra.is_empty() && missing.is_empty(),
        "API role privileges differ: extra {extra:#?}, missing {missing:#?}"
    );
}

fn sql_state(err: &sqlx::Error) -> String {
    err.as_database_error().and_then(|d| d.code()).unwrap_or_default().into_owned()
}

/// `psql`, which runs the split script; `None` (the test is skipped) where it
/// is not installed, except on the Linux CI runners, which have it.
fn psql(test: &str) -> Option<std::process::Command> {
    let found = std::process::Command::new("psql").arg("--version").output().is_ok_and(|o| o.status.success());
    if !found {
        if std::env::var_os("CI").is_some() && cfg!(target_os = "linux") {
            panic!("{test}: psql is not installed");
        }
        eprintln!("{test}: skipped, psql is not installed");
        return None;
    }
    Some(std::process::Command::new("psql"))
}

#[tokio::test]
async fn a_split_single_role_install_grants_the_api_role_what_a_fresh_one_does() {
    const TEST: &str = "a_split_single_role_install_grants_the_api_role_what_a_fresh_one_does";
    let Some(mut psql) = psql(TEST) else { return };
    let Some(roles) = scratch::Roles::create(TEST).await else { return };
    let fresh = roles.database().await;
    // The database rights 00_create_role_and_database.sql sets on a fresh install.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "REVOKE ALL ON DATABASE {db} FROM PUBLIC; GRANT CONNECT ON DATABASE {db} TO {app}, {maintenance}",
        db = fresh.name(),
        app = roles.app,
        maintenance = roles.maintenance
    )))
    .execute(&fresh.pool)
    .await
    .unwrap();
    let expected = api_role_privileges(&fresh.pool).await;
    assert!(expected.iter().any(|p| p == "table cmdb.workflow_instance_events: INSERT"), "{expected:#?}");
    assert!(!expected.iter().any(|p| p.starts_with("table cmdb.workflow_instance_events: UPDATE")), "{expected:#?}");

    // The older bootstrap script: the API role owns the database and migrates it itself.
    let single = roles.empty().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER DATABASE {} OWNER TO {}", single.name(), roles.app)))
        .execute(&single.pool)
        .await
        .unwrap();
    let as_app = roles.api_pool(&single).await;
    MIGRATOR.run(&as_app).await.expect("migrations as the API role");
    as_app.close().await;

    let owner = format!("{}_owner", roles.app.trim_end_matches("_app"));
    let mut url = url::Url::parse(&std::env::var("SHADOUCMDB_TEST_DATABASE_URL").unwrap()).unwrap();
    url.set_path(&format!("/{}", single.name()));
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../sql/bootstrap/10_split_roles.sql");
    let out = psql
        .args(["-X", "-q", "-v", "ON_ERROR_STOP=1", "-d", url.as_str(), "-f", script])
        .args(["-v", &format!("app_role={}", roles.app)])
        .args(["-v", &format!("owner_role={owner}")])
        .args(["-v", &format!("maintenance_role={}", roles.maintenance)])
        .output()
        .expect("run psql");
    assert!(out.status.success(), "10_split_roles.sql failed: {}", String::from_utf8_lossy(&out.stderr));
    // Then `shadoucmdb migrate` as the schema owner, as the script's header says.
    MIGRATOR.run(&single.pool).await.expect("migrate after the split");

    assert_same(&api_role_privileges(&single.pool).await, &expected);

    single.drop().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {owner}"))).execute(&fresh.pool).await.unwrap();
    fresh.drop().await;
    roles.drop().await;
}

#[tokio::test]
async fn the_migration_repairs_an_install_split_before_it() {
    let Some(roles) = scratch::Roles::create("the_migration_repairs_an_install_split_before_it").await else {
        return;
    };
    let fresh = roles.database().await;
    let expected = api_role_privileges(&fresh.pool).await;
    // Nothing a migration narrowed is missing from cmdb.api_role_privileges:
    // granting from the list changes nothing on a fresh install.
    sqlx::query("SELECT cmdb.apply_api_role_grants($1)").bind(&roles.app).execute(&fresh.pool).await.unwrap();
    assert_same(&api_role_privileges(&fresh.pool).await, &expected);

    // What 10_split_roles.sql granted before this release, on top of 0064.
    let db = roles.empty().await;
    MIGRATOR.run_to(64, &db.pool).await.expect("migrations up to 0064");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON cmdb.workflow_instance_events, cmdb.workflow_approval_decisions,
           cmdb.workflow_instance_archive TO {}",
        roles.app
    )))
    .execute(&db.pool)
    .await
    .unwrap();
    MIGRATOR.run(&db.pool).await.expect("migration 0065");
    assert_same(&api_role_privileges(&db.pool).await, &expected);

    let api = roles.api_pool(&db).await;
    // The append-only triggers refuse changes with the same SQLSTATE, so ask for the grants themselves.
    for (table, privileges) in [
        ("cmdb.workflow_instance_events", "UPDATE, DELETE"),
        ("cmdb.workflow_approval_decisions", "UPDATE, DELETE"),
        ("cmdb.workflow_instance_archive", "INSERT, UPDATE, DELETE"),
    ] {
        let any: bool = sqlx::query_scalar("SELECT has_table_privilege(current_user, $1, $2)")
            .bind(table)
            .bind(privileges)
            .fetch_one(&api)
            .await
            .unwrap();
        assert!(!any, "the API role holds one of {privileges} on {table}");
    }
    let err = sqlx::query("SELECT * FROM cmdb.api_role_privileges").execute(&api).await.unwrap_err();
    assert_eq!(sql_state(&err), "42501", "{err}");
    api.close().await;
    db.drop().await;
    fresh.drop().await;
    roles.drop().await;
}
