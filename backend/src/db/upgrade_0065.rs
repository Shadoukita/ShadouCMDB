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

/// A single-role install as the older bootstrap script made it: the API role
/// owns the database and migrated it itself. `plant` then runs as the API role.
async fn single_role_install(roles: &scratch::Roles, plant: &str) -> scratch::Scratch {
    let single = roles.empty().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER DATABASE {} OWNER TO {}", single.name(), roles.app)))
        .execute(&single.pool)
        .await
        .unwrap();
    let as_app = roles.api_pool(&single).await;
    MIGRATOR.run(&as_app).await.expect("migrations as the API role");
    sqlx::raw_sql(sqlx::AssertSqlSafe(plant.to_owned())).execute(&as_app).await.expect("plant as the API role");
    as_app.close().await;
    single
}

/// Runs 10_split_roles.sql as the administrator.
fn split(mut psql: std::process::Command, roles: &scratch::Roles, db: &scratch::Scratch) -> std::process::Output {
    let mut url = url::Url::parse(&std::env::var("SHADOUCMDB_TEST_DATABASE_URL").unwrap()).unwrap();
    url.set_path(&format!("/{}", db.name()));
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../sql/bootstrap/10_split_roles.sql");
    psql.args(["-X", "-q", "-v", "ON_ERROR_STOP=1", "-d", url.as_str(), "-f", script])
        .args(["-v", &format!("app_role={}", roles.app)])
        .args(["-v", &format!("owner_role={}", owner_role(roles))])
        .args(["-v", &format!("maintenance_role={}", roles.maintenance)])
        .output()
        .expect("run psql")
}

fn owner_role(roles: &scratch::Roles) -> String {
    format!("{}_owner", roles.app.trim_end_matches("_app"))
}

async fn is_superuser(pool: &PgPool, role: &str) -> bool {
    sqlx::query_scalar("SELECT rolsuper FROM pg_roles WHERE rolname = $1").bind(role).fetch_one(pool).await.unwrap()
}

async fn grants_function(pool: &PgPool) -> String {
    sqlx::query_scalar("SELECT pg_get_functiondef('cmdb.apply_api_role_grants(name)'::regprocedure)")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_split_single_role_install_grants_the_api_role_what_a_fresh_one_does() {
    const TEST: &str = "a_split_single_role_install_grants_the_api_role_what_a_fresh_one_does";
    let Some(psql) = psql(TEST) else { return };
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

    // A compromised API role replaced the grants function the script calls as
    // the administrator, and added an overload for an untyped argument (GH#725).
    let single = single_role_install(
        &roles,
        &format!(
            "CREATE FUNCTION cmdb.trojan() RETURNS boolean LANGUAGE plpgsql AS $$
               BEGIN RESET ROLE; EXECUTE 'ALTER ROLE {app} SUPERUSER'; RETURN true; END $$;
             CREATE OR REPLACE FUNCTION cmdb.apply_api_role_grants(app_role name) RETURNS void
               LANGUAGE plpgsql AS $$ BEGIN PERFORM cmdb.trojan(); END $$;
             CREATE FUNCTION cmdb.apply_api_role_grants(app_role text) RETURNS void
               LANGUAGE plpgsql AS $$ BEGIN PERFORM cmdb.trojan(); END $$;",
            app = roles.app
        ),
    )
    .await;
    let out = split(psql, &roles, &single);
    assert!(out.status.success(), "10_split_roles.sql failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!is_superuser(&single.pool, &roles.app).await, "the split ran the API role's function");
    assert_eq!(
        grants_function(&single.pool).await,
        grants_function(&fresh.pool).await,
        "the script's copy differs from 0065's"
    );
    // Then `shadoucmdb migrate` as the schema owner, as the script's header says.
    MIGRATOR.run(&single.pool).await.expect("migrate after the split");

    // The split leaves what the API role planted; the check lists it.
    let planted = api_role_privileges(&single.pool).await;
    assert!(planted.iter().any(|p| p == "routine trojan(): EXECUTE (PUBLIC)"), "{planted:#?}");
    sqlx::raw_sql("DROP FUNCTION cmdb.trojan(), cmdb.apply_api_role_grants(text)").execute(&single.pool).await.unwrap();
    assert_same(&api_role_privileges(&single.pool).await, &expected);

    single.drop().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {}", owner_role(&roles)))).execute(&fresh.pool).await.unwrap();
    fresh.drop().await;
    roles.drop().await;
}

#[tokio::test]
async fn the_split_reads_no_table_the_api_role_could_have_made_a_view() {
    const TEST: &str = "the_split_reads_no_table_the_api_role_could_have_made_a_view";
    let Some(_) = psql(TEST) else { return };
    let Some(roles) = scratch::Roles::create(TEST).await else { return };
    let trojan = format!(
        "CREATE FUNCTION cmdb.trojan() RETURNS boolean LANGUAGE plpgsql AS $$
           BEGIN RESET ROLE; EXECUTE 'ALTER ROLE {app} SUPERUSER'; RETURN true; END $$;",
        app = roles.app
    );

    // The area list, which the script used to read to leave the area schemas alone.
    let areas = single_role_install(
        &roles,
        &format!(
            "{trojan}
             ALTER TABLE cmdb.areas RENAME TO areas_data;
             CREATE VIEW cmdb.areas AS SELECT * FROM cmdb.areas_data WHERE cmdb.trojan();"
        ),
    )
    .await;
    let out = split(psql(TEST).unwrap(), &roles, &areas);
    assert!(out.status.success(), "10_split_roles.sql failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!is_superuser(&areas.pool, &roles.app).await, "the split read the API role's view cmdb.areas");

    // The list of the API role's rights: the script refuses it before reading it.
    let list = single_role_install(
        &roles,
        &format!(
            "{trojan}
             ALTER TABLE cmdb.api_role_privileges RENAME TO api_role_privileges_data;
             CREATE VIEW cmdb.api_role_privileges AS SELECT * FROM cmdb.api_role_privileges_data WHERE cmdb.trojan();"
        ),
    )
    .await;
    let out = split(psql(TEST).unwrap(), &roles, &list);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "10_split_roles.sql read a view as the list");
    assert!(stderr.contains("is not the table migration 0065 created"), "{stderr}");
    assert!(!is_superuser(&list.pool, &roles.app).await);

    areas.drop().await;
    list.drop().await;
    let admin = roles.empty().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE {}", owner_role(&roles)))).execute(&admin.pool).await.unwrap();
    admin.drop().await;
    roles.drop().await;
}

#[tokio::test]
async fn the_split_grants_nothing_but_the_list_s_rights_on_cmdb() {
    const TEST: &str = "the_split_grants_nothing_but_the_list_s_rights_on_cmdb";
    let Some(_) = psql(TEST) else { return };
    let Some(roles) = scratch::Roles::create(TEST).await else { return };
    let plants = [
        // SQL in a right, past the CHECK constraint the API role dropped.
        format!(
            "ALTER TABLE cmdb.api_role_privileges DROP CONSTRAINT api_role_privileges_check;
             UPDATE cmdb.api_role_privileges
                SET privileges = ARRAY['SELECT ON cmdb.server_keys TO PUBLIC; ALTER ROLE {app} SUPERUSER; GRANT SELECT']
              WHERE object = 'cmdb.audit_log';",
            app = roles.app
        ),
        // Rights on objects outside cmdb: read any file, every password verifier.
        "INSERT INTO cmdb.api_role_privileges VALUES ('pg_catalog.pg_read_file(text)', 'routine', '{EXECUTE}');"
            .to_owned(),
        "INSERT INTO cmdb.api_role_privileges VALUES ('pg_catalog.pg_authid', 'table', '{SELECT}');".to_owned(),
        // An object type the function does not know.
        "ALTER TABLE cmdb.api_role_privileges DROP CONSTRAINT api_role_privileges_object_type_check,
           DROP CONSTRAINT api_role_privileges_check;
         INSERT INTO cmdb.api_role_privileges VALUES ('cmdb.areas', 'schema', '{}');"
            .to_owned(),
    ];
    for plant in &plants {
        let db = single_role_install(&roles, plant).await;
        let out = split(psql(TEST).unwrap(), &roles, &db);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "10_split_roles.sql accepted the list after {plant}");
        assert!(stderr.contains("cmdb.api_role_privileges: "), "{plant}: {stderr}");
        assert!(!is_superuser(&db.pool, &roles.app).await, "{plant}");
        let rights: (bool, bool) = sqlx::query_as(
            "SELECT has_function_privilege($1, 'pg_catalog.pg_read_file(text)', 'EXECUTE'),
                    has_table_privilege($1, 'pg_catalog.pg_authid', 'SELECT')",
        )
        .bind(&roles.app)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(rights, (false, false), "{plant}");
        db.drop().await;
    }
    // The script stopped before it gave the owner role anything to own.
    let admin = roles.empty().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE IF EXISTS {}", owner_role(&roles))))
        .execute(&admin.pool)
        .await
        .unwrap();
    admin.drop().await;
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

    // A single-role install migrated by another role (a superuser
    // MIGRATION_DATABASE_URL): the API role keeps the rights on the tables it
    // owns until the split, or deleting a CI would fail in the archive trigger.
    let single = roles.empty().await;
    sqlx::query(sqlx::AssertSqlSafe(format!("ALTER DATABASE {} OWNER TO {}", single.name(), roles.app)))
        .execute(&single.pool)
        .await
        .unwrap();
    let as_app = roles.api_pool(&single).await;
    MIGRATOR.run_to(64, &as_app).await.expect("migrations up to 0064 as the API role");
    as_app.close().await;
    MIGRATOR.run(&single.pool).await.expect("migration 0065 as a superuser");
    let api = roles.api_pool(&single).await;
    let insert: bool =
        sqlx::query_scalar("SELECT has_table_privilege(current_user, 'cmdb.workflow_instance_archive', 'INSERT')")
            .fetch_one(&api)
            .await
            .unwrap();
    assert!(insert, "migration 0065 narrowed the API role on a single-role install");
    api.close().await;
    single.drop().await;
    fresh.drop().await;
    roles.drop().await;
}
