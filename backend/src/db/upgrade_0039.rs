//! Migration 0039 (saved views, SHAA-616) against an install with profiles of
//! every kind: exactly the profiles holding `customization.manage` gain
//! `views.share`, no other grant changes, the permission constraint accepts
//! exactly `GlobalPermission::ALL` (T8), and the new tables enforce their rules.

use std::collections::BTreeSet;

use sqlx::{Executor, PgPool};
use uuid::Uuid;

use super::upgrade_0029::{constraint_def, literals};
use crate::auth::permissions::GlobalPermission;
use crate::db::{MIGRATOR, scratch};

const BEFORE: &str = "
INSERT INTO users (id, username, display_name, password_hash) VALUES
  ('00000000-0000-4000-8000-00000000000a', 'alice', 'Alice', '$argon2id$v=19$upgrade-test');
INSERT INTO permission_profiles (id, name) VALUES
  ('00000000-0000-4000-8000-0000000000f1', 'Everything but administrator'),
  ('00000000-0000-4000-8000-0000000000f2', 'Branding'),
  ('00000000-0000-4000-8000-0000000000f3', 'Auditors'),
  ('00000000-0000-4000-8000-0000000000f4', 'Nothing');
INSERT INTO permission_profile_global_permissions (profile_id, permission)
  SELECT '00000000-0000-4000-8000-0000000000f1', p
  FROM unnest(ARRAY['users.manage', 'profiles.manage', 'datamodel.manage', 'customization.manage',
                    'config.export_import', 'audit.view', 'cis.import']) p;
INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES
  ('00000000-0000-4000-8000-0000000000f2', 'customization.manage'),
  ('00000000-0000-4000-8000-0000000000f3', 'audit.view');
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

#[tokio::test]
async fn customisers_gain_views_share_and_nobody_else_does() {
    let Some(db) = scratch::empty("customisers_gain_views_share").await else { return };
    let pool = &db.pool;
    MIGRATOR.run_to(38, pool).await.expect("migrations up to 0038");
    pool.execute(BEFORE).await.expect("data before the upgrade");
    let before = rights(pool).await;
    MIGRATOR.run_to(39, pool).await.expect("migration 0039");

    let after = rights(pool).await;
    let gained: Vec<&(String, String)> = after.iter().filter(|r| !before.contains(r)).collect();
    assert_eq!(
        gained,
        [
            &("Branding".to_owned(), "views.share".to_owned()),
            &("Everything but administrator".to_owned(), "views.share".to_owned()),
        ],
        "exactly the profiles with customization.manage gain views.share"
    );
    assert!(before.iter().all(|r| after.contains(r)), "every existing grant is kept");
    assert_permissions_match(pool).await;

    // The tables start empty and enforce their rules.
    let views: i64 = sqlx::query_scalar("SELECT count(*) FROM saved_views").fetch_one(pool).await.unwrap();
    assert_eq!(views, 0);
    let insert = |owner: Option<&str>, context: &str, name: &str| {
        sqlx::query(
            "INSERT INTO saved_views (owner_id, context, name, definition, created_by_name, updated_by_name)
             VALUES ($1::uuid, $2, $3, '{}', 'alice', 'alice') RETURNING id",
        )
        .bind(owner.map(str::to_owned))
        .bind(context.to_owned())
        .bind(name.to_owned())
        .fetch_one(pool)
    };
    let alice = Some("00000000-0000-4000-8000-00000000000a");
    let personal: Uuid = sqlx::Row::get(&insert(alice, "inventory", "Servers").await.unwrap(), 0);
    assert!(insert(alice, "inventory", "SERVERS").await.is_err(), "names are unique per owner, ignoring case");
    assert!(insert(alice, "search", "Servers").await.is_ok(), "per context");
    assert!(insert(None, "inventory", "Servers").await.is_ok(), "shared names are their own namespace");
    assert!(insert(None, "inventory", "servers").await.is_err());
    assert!(insert(alice, "report", "x").await.is_err(), "unknown context");
    assert!(insert(alice, "inventory", "  ").await.is_err(), "blank name");

    let default = |context: &str, home: &str| {
        sqlx::query("INSERT INTO saved_view_defaults (user_id, context, home, view_id) VALUES ($1::uuid, $2, $3, $4)")
            .bind(alice.map(str::to_owned))
            .bind(context.to_owned())
            .bind(home.to_owned())
            .bind(personal)
            .execute(pool)
    };
    assert!(default("search", "").await.is_err(), "search views are never a default (D7)");
    assert!(default("inventory", "Not a key").await.is_err());
    assert!(default("inventory", "server").await.is_ok());
    assert!(default("inventory", "").await.is_ok());

    // Deleting the user removes their personal views and defaults; shared views stay.
    pool.execute("DELETE FROM users WHERE username = 'alice'").await.unwrap();
    let left: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM saved_views), (SELECT count(*) FROM saved_view_defaults)")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(left, (1, 0));

    // Running the migrations again is a no-op.
    MIGRATOR.run(pool).await.expect("re-run");
    assert_eq!(rights(pool).await.len(), after.len());
    db.drop().await;
}
