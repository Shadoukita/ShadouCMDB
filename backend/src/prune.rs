//! `shadoucmdb prune-audit`: apply the audit_log retention window.
//!
//! The deletion itself is `cmdb.prune_audit_log()` (migrations 0007, 0008), a SECURITY
//! DEFINER function that only the maintenance role may execute. It deletes by
//! age only, refuses windows under 30 days, and records every real run as an
//! `audit.purge` row. This command connects with MAINTENANCE_DATABASE_URL and
//! never with the API's DATABASE_URL, and it only reports unless `--execute`.

use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Args, ValueEnum};
use sqlx::PgConnection;

use crate::config::Config;
use crate::db;

/// Shortest window the database accepts; mirrored here for a friendlier error.
pub const MIN_DAYS: u32 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Scope {
    /// Sign-in, sign-out, session and API token use events (IP address, user
    /// agent), plus sessions that expired more than 30 days ago.
    Auth,
    /// CI and configuration change history (create, update, delete, restore).
    Changes,
}

impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Scope::Auth => "auth",
            Scope::Changes => "changes",
        }
    }
}

#[derive(Debug, Args)]
pub struct PruneAuditArgs {
    /// Delete rows older than this many days, e.g. `180d` (the retention policy for
    /// authentication events). At least 30 days.
    #[arg(long, value_name = "DAYS", value_parser = parse_days)]
    pub older_than: u32,
    /// Which rows: `auth` (authentication events) or `changes` (change history, kept
    /// indefinitely unless you prune it explicitly).
    #[arg(long, value_enum, default_value = "auth")]
    pub scope: Scope,
    /// Actually delete. Without it the command only counts what it would delete.
    #[arg(long)]
    pub execute: bool,
    /// Only count (the default; accepted for scripts that want to say so).
    #[arg(long, conflicts_with = "execute")]
    pub dry_run: bool,
}

/// `180d` or `180`.
fn parse_days(raw: &str) -> Result<u32, String> {
    let digits = raw.trim().strip_suffix(['d', 'D']).unwrap_or(raw.trim());
    let days: u32 = digits.parse().map_err(|_| format!("expected a number of days such as 180d, got \"{raw}\""))?;
    if days < MIN_DAYS {
        return Err(format!("the window must be at least {MIN_DAYS} days"));
    }
    Ok(days)
}

/// Runs prune_audit_log() on `conn`; one (category, rows) pair per action, plus `sessions` for the auth scope.
pub async fn prune(
    conn: &mut PgConnection,
    days: u32,
    scope: Scope,
    dry_run: bool,
    operator: Option<&str>,
) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as("SELECT category, total FROM prune_audit_log(make_interval(days => $1), $2, $3, $4)")
        .bind(i32::try_from(days).unwrap_or(i32::MAX))
        .bind(scope.as_str())
        .bind(dry_run)
        .bind(operator)
        .fetch_all(conn)
        .await
}

/// Who ran the command, as the operating system says; recorded next to the database user.
fn operator() -> Option<String> {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).ok().filter(|u| !u.is_empty())
}

pub async fn run(cfg: &Config, args: PruneAuditArgs) -> anyhow::Result<()> {
    let Some(url) = &cfg.maintenance_url else {
        bail!(
            "MAINTENANCE_DATABASE_URL is not set. prune-audit connects as the maintenance role \
             (shadoucmdb_maintenance), never as the API's DATABASE_URL user; see docs/deployment.md"
        );
    };
    let mut db = cfg.database.with_url(url);
    // A large first purge can take longer than the API's per-statement limit.
    db.statement_timeout = Duration::ZERO;
    db.pool_max = 1;
    let pool = db::connect(&db).await?;
    let dry_run = !args.execute;
    let result = async {
        let mut conn = pool.acquire().await?;
        let rows =
            prune(&mut conn, args.older_than, args.scope, dry_run, operator().as_deref()).await.map_err(explain)?;
        report(&args, dry_run, &rows);
        anyhow::Ok(())
    }
    .await;
    pool.close().await;
    result.context("prune-audit")
}

/// The two failures an operator can fix, in their terms.
fn explain(e: sqlx::Error) -> anyhow::Error {
    match e.as_database_error().and_then(|d| d.code()).as_deref() {
        Some("42883") => anyhow::anyhow!("prune_audit_log() does not exist; run `shadoucmdb migrate` first"),
        Some("42501") => anyhow::anyhow!(
            "the MAINTENANCE_DATABASE_URL user may not execute prune_audit_log(); it must connect as \
             shadoucmdb_maintenance (see docs/deployment.md, \"Database roles\")"
        ),
        _ => anyhow::Error::new(e).context("prune_audit_log() failed"),
    }
}

fn report(args: &PruneAuditArgs, dry_run: bool, rows: &[(String, i64)]) {
    let verb = if dry_run { "Would delete" } else { "Deleted" };
    println!("{verb} audit_log rows in scope \"{}\" older than {} days:", args.scope.as_str(), args.older_than);
    let audit: Vec<_> = rows.iter().filter(|(c, _)| c != "sessions").collect();
    if audit.is_empty() {
        println!("  (none)");
    }
    for (category, n) in audit {
        println!("  {category:<16} {n}");
    }
    if let Some((_, n)) = rows.iter().find(|(c, _)| c == "sessions") {
        println!("{verb} {n} sessions that expired more than 30 days ago");
    }
    if dry_run {
        println!("Dry run: nothing was deleted. Re-run with --execute to delete these rows.");
    } else {
        println!("Recorded as an audit.purge entry in audit_log.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::scratch;
    use sqlx::{Connection, Executor};
    use std::str::FromStr;

    #[test]
    fn days_accept_a_suffix_and_enforce_the_floor() {
        assert_eq!(parse_days("180d"), Ok(180));
        assert_eq!(parse_days("30"), Ok(30));
        assert!(parse_days("29d").is_err());
        assert!(parse_days("6m").is_err());
        assert!(parse_days("-1").is_err());
    }

    /// Roles are cluster-wide. Create the two the migration grants to (without
    /// LOGIN; the test uses SET ROLE) before the scratch database is migrated.
    async fn ensure_roles() -> bool {
        let Ok(url) = std::env::var("SHADOUCMDB_TEST_DATABASE_URL") else { return false };
        let opts = sqlx::postgres::PgConnectOptions::from_str(&url).unwrap();
        let mut c = sqlx::postgres::PgConnection::connect_with(&opts).await.unwrap();
        for role in ["shadoucmdb_app", "shadoucmdb_maintenance"] {
            c.execute(sqlx::AssertSqlSafe(format!(
                // unique_violation: another test created it concurrently.
                "DO $$ BEGIN CREATE ROLE {role} NOLOGIN;
                 EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
            )))
            .await
            .unwrap();
        }
        c.close().await.ok();
        true
    }

    async fn sqlstate<T>(c: &mut PgConnection, result: sqlx::Result<T>) -> String {
        let err = result.err().expect("statement should have been rejected");
        c.execute("ROLLBACK TO SAVEPOINT sp").await.unwrap();
        err.as_database_error().and_then(|d| d.code()).unwrap_or_default().into_owned()
    }

    async fn count(c: &mut PgConnection, filter: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT count(*) FROM audit_log WHERE {filter}")))
            .fetch_one(c)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn only_the_maintenance_role_can_prune_and_the_api_role_cannot_delete() {
        const TEST: &str = "only_the_maintenance_role_can_prune_and_the_api_role_cannot_delete";
        if !ensure_roles().await {
            scratch::database(TEST).await; // prints the skip notice (or fails in CI)
            return;
        }
        let Some(db) = scratch::database(TEST).await else { return };
        let mut c = db.pool.acquire().await.unwrap();

        // As the schema owner: one old and one recent row per scope, and two expired sessions.
        c.execute(
            "INSERT INTO audit_log (actor_type, action, entity_type, entity_id, new_value, occurred_at) VALUES
               ('system', 'login.failure', 'sessions', gen_random_uuid(), '{\"ipAddress\":\"192.0.2.1\"}', now() - interval '200 days'),
               ('system', 'login.success', 'sessions', gen_random_uuid(), '{\"ipAddress\":\"192.0.2.2\"}', now() - interval '10 days'),
               ('system', 'token.use', 'api_tokens', gen_random_uuid(), '{\"ipAddress\":\"192.0.2.3\"}', now() - interval '200 days'),
               ('system', 'create', 'configuration_items', gen_random_uuid(), '{}', now() - interval '400 days'),
               ('system', 'create', 'configuration_items', gen_random_uuid(), '{}', now());
             INSERT INTO users (username, display_name, password_hash) VALUES ('p', 'p', '$argon2id$v=19$test');
             INSERT INTO sessions (token_hash, user_id, csrf_token, expires_at, last_seen_at)
               SELECT sha256(v::bytea), (SELECT id FROM users), 'x', now() - d, now() - d
               FROM (VALUES ('a', interval '40 days'), ('b', interval '10 days')) s(v, d);",
        )
        .await
        .unwrap();

        // The API role: no UPDATE, DELETE or TRUNCATE on audit_log, even with the
        // purge variable set, and no EXECUTE on the purge function.
        c.execute("BEGIN; SET LOCAL ROLE shadoucmdb_app").await.unwrap();
        for stmt in [
            "UPDATE audit_log SET actor_name = 'tampered'",
            "DELETE FROM audit_log",
            "TRUNCATE audit_log",
            "SELECT set_config('shadoucmdb.audit_purge', 'on', true); DELETE FROM audit_log",
        ] {
            c.execute("SAVEPOINT sp").await.unwrap();
            let r = c.execute(sqlx::AssertSqlSafe(stmt)).await;
            assert_eq!(sqlstate(&mut c, r).await, "42501", "API role: {stmt}");
        }
        c.execute("SAVEPOINT sp").await.unwrap();
        let r = prune(&mut c, 180, Scope::Auth, true, None).await;
        assert_eq!(sqlstate(&mut c, r).await, "42501", "API role: prune_audit_log()");
        c.execute("ROLLBACK").await.unwrap();

        // The maintenance role: no direct DELETE, a floor of 30 days, a dry run that deletes nothing.
        c.execute("SET ROLE shadoucmdb_maintenance").await.unwrap();
        c.execute("BEGIN; SAVEPOINT sp").await.unwrap();
        let r = c.execute("DELETE FROM audit_log").await;
        assert_eq!(sqlstate(&mut c, r).await, "42501", "maintenance role: DELETE");
        let r = prune(&mut c, 29, Scope::Auth, false, None).await;
        assert_eq!(sqlstate(&mut c, r).await, "22023", "window under 30 days");
        c.execute("COMMIT").await.unwrap();

        let dry = prune(&mut c, 180, Scope::Auth, true, Some("tester")).await.unwrap();
        assert_eq!(dry, vec![("login.failure".into(), 1), ("token.use".into(), 1), ("sessions".into(), 1)]);
        c.execute("RESET ROLE").await.unwrap();
        assert_eq!(count(&mut c, "true").await, 5, "a dry run deletes nothing and records nothing");

        c.execute("SET ROLE shadoucmdb_maintenance").await.unwrap();
        let done = prune(&mut c, 180, Scope::Auth, false, Some("tester")).await.unwrap();
        assert_eq!(done, dry);
        let changes = prune(&mut c, 365, Scope::Changes, false, Some("tester")).await.unwrap();
        assert_eq!(changes, vec![("create".into(), 1)]);
        c.execute("RESET ROLE").await.unwrap();

        assert_eq!(count(&mut c, "action = 'login.failure'").await, 0);
        assert_eq!(count(&mut c, "action = 'token.use'").await, 0, "token use is an access event");
        assert_eq!(count(&mut c, "action = 'login.success'").await, 1, "inside the window");
        assert_eq!(count(&mut c, "action = 'create'").await, 1, "inside the window");
        let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions").fetch_one(&mut *c).await.unwrap();
        assert_eq!(sessions, 1, "only the session expired more than 30 days ago is gone");

        let purges: Vec<serde_json::Value> =
            sqlx::query_scalar("SELECT new_value FROM audit_log WHERE action = 'audit.purge' ORDER BY id")
                .fetch_all(&mut *c)
                .await
                .unwrap();
        let login: String = sqlx::query_scalar("SELECT session_user::text").fetch_one(&mut *c).await.unwrap();
        assert_eq!(purges.len(), 2, "one audit.purge row per executed run, none for the dry run");
        assert_eq!(purges[0]["scope"], "auth");
        assert_eq!(purges[0]["deleted"], serde_json::json!({ "login.failure": 1, "token.use": 1 }));
        assert_eq!(purges[0]["sessionsDeleted"], serde_json::json!(1));
        assert_eq!(purges[0]["operator"], "tester");
        assert_eq!(purges[0]["databaseUser"], login.as_str());
        assert_eq!(purges[1]["scope"], "changes");

        // Even the owner, with the purge variable set, cannot delete an audit.purge row or update anything.
        c.execute("BEGIN; SELECT set_config('shadoucmdb.audit_purge', 'on', true); SAVEPOINT sp").await.unwrap();
        let r = c.execute("DELETE FROM audit_log WHERE action = 'audit.purge'").await;
        assert_eq!(sqlstate(&mut c, r).await, "42501", "audit.purge rows are never deleted");
        let r = c.execute("UPDATE audit_log SET actor_name = 'tampered'").await;
        assert_eq!(sqlstate(&mut c, r).await, "42501", "UPDATE is never allowed");
        c.execute("ROLLBACK").await.unwrap();

        drop(c);
        db.drop().await;
    }

    #[tokio::test]
    async fn a_function_planted_in_public_does_not_run_with_the_owners_rights() {
        const TEST: &str = "a_function_planted_in_public_does_not_run_with_the_owners_rights";
        if !ensure_roles().await {
            scratch::database(TEST).await;
            return;
        }
        let Some(db) = scratch::database(TEST).await else { return };
        let mut c = db.pool.acquire().await.unwrap();
        let privilege = |sql: &'static str| sqlx::query_scalar::<_, bool>(sql);

        let open = privilege(
            "SELECT has_schema_privilege('public', 'public', 'CREATE')
                 OR has_schema_privilege('shadoucmdb_app', 'public', 'CREATE')",
        );
        assert!(!open.fetch_one(&mut *c).await.unwrap(), "only the owner may create objects in public");

        // Reopen public as PostgreSQL 14 ships it. The API role plants a jsonb_object_agg(text, bigint)
        // aggregate, a closer match than pg_catalog's ("any", "any"); if prune_audit_log() resolved it,
        // its state function would run as the schema owner and grant the API role DELETE on audit_log.
        c.execute(
            "INSERT INTO audit_log (actor_type, action, entity_type, entity_id, new_value, occurred_at)
               VALUES ('system', 'login.failure', 'sessions', gen_random_uuid(), '{}', now() - interval '200 days');
             GRANT CREATE ON SCHEMA public TO shadoucmdb_app;
             SET ROLE shadoucmdb_app;
             CREATE FUNCTION public.hijack(jsonb, text, bigint) RETURNS jsonb LANGUAGE plpgsql AS $$
               BEGIN GRANT UPDATE, DELETE ON cmdb.audit_log TO shadoucmdb_app; RETURN $1; END $$;
             CREATE AGGREGATE public.jsonb_object_agg(text, bigint) (sfunc = public.hijack, stype = jsonb);
             SET ROLE shadoucmdb_maintenance;",
        )
        .await
        .unwrap();
        let dry = prune(&mut c, 180, Scope::Auth, true, None).await.unwrap();
        let done = prune(&mut c, 180, Scope::Auth, false, None).await.unwrap();
        c.execute("RESET ROLE").await.unwrap();

        let gained = privilege(
            "SELECT has_table_privilege('shadoucmdb_app', 'cmdb.audit_log', 'UPDATE')
                 OR has_table_privilege('shadoucmdb_app', 'cmdb.audit_log', 'DELETE')",
        );
        assert!(!gained.fetch_one(&mut *c).await.unwrap(), "the planted aggregate ran as the owner");
        assert_eq!(dry, vec![("login.failure".into(), 1), ("sessions".into(), 0)]);
        assert_eq!(done, dry, "the built-in aggregate counted the rows");

        // The 30-day floor holds for month and year intervals too: it is checked on the resulting
        // cutoff, so '1 month' is refused exactly when the previous month was shorter than 30 days.
        let short = privilege("SELECT now() - interval '1 month' > now() - interval '30 days'");
        let month_is_short = short.fetch_one(&mut *c).await.unwrap();
        c.execute("SET ROLE shadoucmdb_maintenance").await.unwrap();
        for (window, refused) in [("1 month", month_is_short), ("1 year -340 days", true), ("29 days 23:59", true)] {
            c.execute("BEGIN; SAVEPOINT sp").await.unwrap();
            let r = sqlx::query("SELECT * FROM prune_audit_log($1::interval, 'auth', true)")
                .bind(window)
                .execute(&mut *c)
                .await;
            if refused {
                assert_eq!(sqlstate(&mut c, r).await, "22023", "window {window}");
            } else {
                r.unwrap();
            }
            c.execute("ROLLBACK").await.unwrap();
        }
        c.execute("RESET ROLE").await.unwrap();

        drop(c);
        db.drop().await;
    }
}
