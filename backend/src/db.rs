//! Connection pool and migrations. This binary is the only database client.

use std::collections::HashSet;
use std::str::FromStr;

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};

use crate::config::{DatabaseConfig, SslMode};

/// `sql/migrations/*.sql`, embedded at compile time. The folder is the single
/// source of truth for the schema; build.rs makes cargo rebuild when it changes.
/// The bookkeeping table is named with its schema: the connection's search_path
/// starts with `cmdb`, and the table has always lived in `public`.
pub static MIGRATOR: Migrator = Migrator {
    table_name: std::borrow::Cow::Borrowed("public._sqlx_migrations"),
    ..sqlx::migrate!("../sql/migrations")
};

/// Where sqlx records applied migrations.
const MIGRATIONS_TABLE: &str = "public._sqlx_migrations";

/// System tables live in `cmdb` (migration 0008); `public` holds the pg_trgm
/// functions. Admin-defined areas are separate schemas, always schema-qualified.
pub const SEARCH_PATH: &str = "cmdb, public";
/// Where the Node/Drizzle runner recorded them before this binary existed.
const DRIZZLE_TABLE: &str = "drizzle.__drizzle_migrations";

pub fn connect_options(cfg: &DatabaseConfig) -> anyhow::Result<PgConnectOptions> {
    let mut opts = match &cfg.url {
        Some(url) => {
            PgConnectOptions::from_str(url).context("DATABASE_URL is not a valid PostgreSQL connection string")?
        }
        None => {
            // Config validation guarantees host, database and user are present here.
            let mut o = PgConnectOptions::new()
                .host(cfg.host.as_deref().unwrap_or_default())
                .port(cfg.port)
                .database(cfg.database.as_deref().unwrap_or_default())
                .username(cfg.user.as_deref().unwrap_or_default());
            if let Some(pw) = &cfg.password {
                o = o.password(pw);
            }
            o
        }
    };

    // DATABASE_SSL is the single source of truth: any sslmode in the URL is overridden.
    opts = opts.ssl_mode(match cfg.ssl {
        SslMode::Disable => PgSslMode::Disable,
        SslMode::Require => PgSslMode::Require,
        SslMode::VerifyFull => PgSslMode::VerifyFull,
    });
    if let Some(ca) = &cfg.ssl_ca_file {
        opts = opts.ssl_root_cert(ca);
    }
    if cfg.ssl == SslMode::Require && opts.get_socket().is_none() && !is_loopback_host(opts.get_host()) {
        tracing::warn!(
            host = opts.get_host(),
            "DATABASE_SSL=require does not verify the database server's certificate; anyone on the network path \
             can impersonate it. Use DATABASE_SSL=verify-full (with DATABASE_SSL_CA_FILE for a private CA)"
        );
    }

    opts = opts.application_name("shadoucmdb").options([("search_path", SEARCH_PATH)]);
    if !cfg.statement_timeout.is_zero() {
        opts = opts.options([("statement_timeout", cfg.statement_timeout.as_millis().to_string())]);
    }
    Ok(opts)
}

/// A host that never leaves this machine: a Unix socket directory, `localhost` or a loopback address.
fn is_loopback_host(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    host.starts_with('/')
        || bare.eq_ignore_ascii_case("localhost")
        || bare.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn pool_options(cfg: &DatabaseConfig) -> PgPoolOptions {
    PgPoolOptions::new()
        .max_connections(cfg.pool_max)
        .min_connections(0)
        // Fail fast instead of hanging requests (and /readyz) when the database is unreachable.
        .acquire_timeout(cfg.connect_timeout)
}

/// Pool that connects on first use, so the server starts (and reports
/// not-ready) while the database is still unreachable.
pub fn lazy_pool(cfg: &DatabaseConfig) -> anyhow::Result<PgPool> {
    Ok(pool_options(cfg).connect_lazy_with(connect_options(cfg)?))
}

/// Pool with one connection established up front, for the CLI commands.
pub async fn connect(cfg: &DatabaseConfig) -> anyhow::Result<PgPool> {
    pool_options(cfg).connect_with(connect_options(cfg)?).await.context("could not connect to PostgreSQL")
}

async fn table_exists(pool: &PgPool, name: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL").bind(name).fetch_one(pool).await
}

/// Versions recorded as successfully applied (empty on a fresh database).
pub async fn applied_versions(pool: &PgPool) -> sqlx::Result<HashSet<i64>> {
    if !table_exists(pool, MIGRATIONS_TABLE).await? {
        return Ok(HashSet::new());
    }
    let rows: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM public._sqlx_migrations WHERE success").fetch_all(pool).await?;
    Ok(rows.into_iter().collect())
}

/// Number of migrations shipped with this build.
pub fn expected_count() -> usize {
    MIGRATOR.iter().filter(|m| m.migration_type.is_up_migration()).count()
}

/// Number of this build's migrations that are applied.
pub async fn applied_count(pool: &PgPool) -> sqlx::Result<usize> {
    let applied = applied_versions(pool).await?;
    Ok(MIGRATOR.iter().filter(|m| applied.contains(&m.version)).count())
}

/// Remembers once the schema is known to be current, so the `/api` gate costs
/// an atomic load per request after that. Until then (a `serve` started
/// before `migrate`) each API request re-checks, so running `migrate` against
/// a live server takes effect without a restart. Migrations only move
/// forward while the server runs, so "current" is never unset.
#[derive(Default)]
pub struct SchemaState {
    current: std::sync::atomic::AtomicBool,
}

/// Outcome of [`SchemaState::check`].
pub enum SchemaCheck {
    Current,
    Pending { applied: usize, expected: usize },
}

impl SchemaState {
    pub async fn check(&self, pool: &PgPool) -> sqlx::Result<SchemaCheck> {
        use std::sync::atomic::Ordering;
        if self.current.load(Ordering::Relaxed) {
            return Ok(SchemaCheck::Current);
        }
        let (applied, expected) = (applied_count(pool).await?, expected_count());
        if applied < expected {
            return Ok(SchemaCheck::Pending { applied, expected });
        }
        self.current.store(true, Ordering::Relaxed);
        Ok(SchemaCheck::Current)
    }
}

fn label(m: &sqlx::migrate::Migration) -> String {
    format!("{:04}_{}", m.version, m.description.replace(' ', "_"))
}

pub async fn migrate(cfg: &DatabaseConfig, adopt_drizzle: bool) -> anyhow::Result<()> {
    let pool = connect(cfg).await?;
    let result = migrate_with(&pool, cfg, adopt_drizzle).await;
    pool.close().await;
    result
}

/// Three-role install: switches the transaction to the API role, which owns
/// the area schemas and type tables and alters them at run time (migration
/// 0008 checked the membership). Returns whether it switched; `RESET ROLE`
/// switches back.
pub async fn act_as_api_role(conn: &mut sqlx::PgConnection) -> sqlx::Result<bool> {
    let as_api_role: bool = sqlx::query_scalar(
        "SELECT current_user <> 'shadoucmdb_app' AND pg_has_role(current_user, 'shadoucmdb_app', 'MEMBER')
         FROM pg_roles WHERE rolname = 'shadoucmdb_app'",
    )
    .fetch_optional(&mut *conn)
    .await?
    .unwrap_or(false);
    if as_api_role {
        sqlx::query("SET LOCAL ROLE shadoucmdb_app").execute(&mut *conn).await?;
    }
    Ok(as_api_role)
}

async fn migrate_with(pool: &PgPool, cfg: &DatabaseConfig, adopt_drizzle: bool) -> anyhow::Result<()> {
    let (db, version): (String, String) =
        sqlx::query_as("SELECT current_database(), current_setting('server_version')").fetch_one(pool).await?;
    println!("Connected to database \"{db}\" (PostgreSQL {version}), ssl={}", cfg.ssl.as_str());

    let mut applied = applied_versions(pool).await?;
    if applied.is_empty() && table_exists(pool, DRIZZLE_TABLE).await? {
        if !adopt_drizzle {
            bail!(
                "this database was migrated by the old Node/Drizzle runner ({DRIZZLE_TABLE} exists) and has no \
                 {MIGRATIONS_TABLE} table yet.\nRun `shadoucmdb migrate --adopt-drizzle` once to record those \
                 migrations for this binary, or migrate an empty database. See sql/README.md."
            );
        }
        adopt_drizzle_history(pool).await?;
        applied = applied_versions(pool).await?;
    }

    let expected = expected_count();
    let pending: Vec<_> =
        MIGRATOR.iter().filter(|m| m.migration_type.is_up_migration() && !applied.contains(&m.version)).collect();
    println!("Migrations: {expected} in binary, {} applied, {} pending", expected - pending.len(), pending.len());

    // Each pending migration runs in its own transaction together with its
    // bookkeeping row, under an advisory lock; re-running is a no-op.
    MIGRATOR.run(pool).await.map_err(|e| {
        let denied = e.to_string().contains("permission denied");
        let err = anyhow::Error::new(e).context("migration failed");
        if denied {
            // The usual cause on a three-role install: migrating with the API's DATABASE_URL.
            err.context("this database user may not change the schema; set MIGRATION_DATABASE_URL to the schema owner (shadoucmdb_owner), see docs/deployment.md")
        } else {
            err
        }
    })?;

    for m in &pending {
        println!("  applied {}", label(m));
    }
    let after = applied_count(pool).await?;

    // Area schemas, type tables and reporting views follow the data model; bring
    // anything missing (after migration 0009, or a new reporting role) in line.
    let ctx = crate::api::context::RequestContext::system("migrate", "migrate");
    let mut tx = pool.begin().await?;
    act_as_api_role(&mut tx).await?;
    let change = crate::schema::reconcile(&mut tx, &ctx, "Reconcile after migrate")
        .await
        .map_err(|e| anyhow::anyhow!("reconciling the data model failed: {}", e.message))?;
    tx.commit().await?;
    let reconciled = match &change {
        Some(c) => {
            println!("Data model: {} statements applied (reporting views and grants)", c.statements.len());
            for i in c.impact.0.iter().filter(|i| i.kind == "warning") {
                println!("  warning: {}", i.message);
            }
            true
        }
        None => false,
    };
    println!(
        "Database is at migration {after}/{expected}{}",
        if pending.is_empty() && !reconciled { " (nothing to do)" } else { "" }
    );
    Ok(())
}

/// One-time hand-over from the Node/Drizzle runner: checks that every row in
/// drizzle.__drizzle_migrations is the SHA-256 of the matching embedded
/// migration (same order), then records those migrations as applied without
/// running them. Leaves the drizzle schema in place.
async fn adopt_drizzle_history(pool: &PgPool) -> anyhow::Result<()> {
    let hashes: Vec<String> =
        sqlx::query_scalar("SELECT hash FROM drizzle.__drizzle_migrations ORDER BY created_at, id")
            .fetch_all(pool)
            .await?;
    let ours: Vec<_> = MIGRATOR.iter().filter(|m| m.migration_type.is_up_migration()).collect();
    if hashes.len() > ours.len() {
        bail!(
            "{DRIZZLE_TABLE} lists {} migrations but this binary only knows {}; use a newer binary",
            hashes.len(),
            ours.len()
        );
    }
    for (i, (hash, m)) in hashes.iter().zip(&ours).enumerate() {
        let sha = hex::encode(Sha256::digest(m.sql.as_str().as_bytes()));
        if *hash != sha {
            bail!(
                "drizzle migration #{i} does not match {} (hash {hash} vs {sha}); reset the database instead",
                label(m)
            );
        }
    }
    let Some(last) = hashes.len().checked_sub(1).map(|i| ours[i].version) else {
        println!("Drizzle history is empty; nothing to adopt");
        return Ok(());
    };
    MIGRATOR.skip(pool, Some(last)).await.context("recording adopted migrations failed")?;
    println!(
        "Adopted {} migrations from {DRIZZLE_TABLE} (up to {}); the drizzle schema can be dropped later",
        hashes.len(),
        label(ours[hashes.len() - 1])
    );
    Ok(())
}

/// Throwaway databases for tests that need a real PostgreSQL.
///
/// Opt-in: set `SHADOUCMDB_TEST_DATABASE_URL` to a connection string whose
/// user may `CREATE DATABASE` (CI does). Each test gets its own freshly
/// migrated database, dropped again by [`Scratch::drop`]. Without the
/// variable the tests print a notice and pass, except in CI (`CI` set), where
/// a silent skip would mean the regression coverage quietly stopped running:
/// there they fail unless `SHADOUCMDB_SKIP_DB_TESTS=1` opts out explicitly.
#[cfg(test)]
pub mod scratch {
    use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
    use sqlx::{ConnectOptions, Connection, Executor};
    use std::str::FromStr;

    pub struct Scratch {
        admin: PgConnectOptions,
        name: String,
        pub pool: PgPool,
    }

    pub async fn database(test: &str) -> Option<Scratch> {
        let db = empty(test).await?;
        super::MIGRATOR.run(&db.pool).await.expect("migrations");
        Some(db)
    }

    /// A database with no migrations applied, as after `CREATE DATABASE`.
    pub async fn empty(test: &str) -> Option<Scratch> {
        let Ok(url) = std::env::var("SHADOUCMDB_TEST_DATABASE_URL") else {
            let opted_out = std::env::var("SHADOUCMDB_SKIP_DB_TESTS").is_ok_and(|v| v == "1");
            if std::env::var_os("CI").is_some() && !opted_out {
                panic!(
                    "{test}: SHADOUCMDB_TEST_DATABASE_URL is not set in CI (set SHADOUCMDB_SKIP_DB_TESTS=1 to skip)"
                );
            }
            eprintln!("{test}: skipped, SHADOUCMDB_TEST_DATABASE_URL is not set");
            return None;
        };
        let admin = PgConnectOptions::from_str(&url).expect("SHADOUCMDB_TEST_DATABASE_URL");
        let name = format!("shadoucmdb_test_{}", uuid::Uuid::new_v4().simple());
        let mut c = admin.connect().await.expect("connect to SHADOUCMDB_TEST_DATABASE_URL");
        c.execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}"))).await.expect("CREATE DATABASE");
        c.close().await.ok();
        // The same search_path as the application's pool (system tables live in `cmdb`).
        let opts = admin.clone().database(&name).options([("search_path", super::SEARCH_PATH)]);
        let pool = PgPoolOptions::new().max_connections(8).connect_with(opts).await.unwrap();
        Some(Scratch { admin, name, pool })
    }

    impl Scratch {
        pub async fn drop(self) {
            self.pool.close().await;
            let mut c = self.admin.connect().await.unwrap();
            c.execute(sqlx::AssertSqlSafe(format!("DROP DATABASE {} WITH (FORCE)", self.name))).await.unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_loopback_host;

    #[test]
    fn loopback_hosts_are_the_ones_that_never_leave_the_machine() {
        for local in ["localhost", "LOCALHOST", "127.0.0.1", "127.8.9.10", "::1", "[::1]", "/var/run/postgresql"] {
            assert!(is_loopback_host(local), "{local}");
        }
        for remote in ["db.example.internal", "10.0.0.5", "::ffff:10.0.0.5", "localhost.example.com", ""] {
            assert!(!is_loopback_host(remote), "{remote}");
        }
    }
}

#[cfg(test)]
mod upgrade_0016;
#[cfg(test)]
mod upgrade_0017;
