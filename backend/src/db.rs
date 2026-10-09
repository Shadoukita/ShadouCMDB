//! Connection pool and migrations. This binary is the only database client.

use std::collections::HashSet;
use std::str::FromStr;

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};

use crate::config::{DatabaseConfig, RoleNames, SslMode};

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
/// Earlier text of migrations changed after they were applied by pre-release
/// builds, never in a release: (version, SHA-384 of that text). 0007 to 0009
/// granted to the default role names only (GH#42); for those names the result
/// is the same. 0036 could overwrite a Criticality set while it ran (GH#391);
/// where nothing was written meanwhile the result is the same. 0041 removed the
/// member type's rules without an audit row (GH#515); the rules are removed
/// either way. `migrate` records the current checksum over these and `restore`
/// accepts backups that carry them.
pub const SUPERSEDED_CHECKSUMS: &[(i64, &str)] = &[
    (7, "3b8728b4ac11de7d08e7ed9a8946be7ca8cb75d95506da572a9c3de69b7d52df159a4b3650acc6c5aed15803d7b73766"),
    (8, "b747f291dc6282d181e8ed1350318a82b442995542594e29da589acd37586929e8da3886ded145b5519a0c8c109d2767"),
    (9, "874f895b5878b4e1dd0d2c3cd841b099e500a70bd7d7a5f3cd6d7f030a8da10adc55ecfc4e8d337beeb326ac88e7f1d4"),
    (36, "8dbe56aa7993fca7f71cf8f21ea87b98d2d55e7f6a5a182f535901b7cddef7fac5da4f28d2626539ebe07ee43d1079cd"),
    (41, "e2ffb21d9512840ba47272874a7f4a40283229c97babc907664966091c3aced38749ec518e3ed48ef30e1e56e2ec20c0"),
];

/// Whether `checksum` (hex) is this binary's migration `version` or a superseded text of it.
pub fn checksum_matches(version: i64, checksum: &str) -> bool {
    MIGRATOR.iter().any(|m| m.version == version && hex::encode(&*m.checksum) == checksum)
        || SUPERSEDED_CHECKSUMS.iter().any(|&(v, sum)| v == version && sum == checksum)
}

/// Where the Node/Drizzle runner recorded them before this binary existed.
const DRIZZLE_TABLE: &str = "drizzle.__drizzle_migrations";

/// The connection string or PG* values alone, before TLS and session settings.
fn base_options(cfg: &DatabaseConfig) -> anyhow::Result<PgConnectOptions> {
    Ok(match &cfg.url {
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
    })
}

/// The database user a configuration connects as (libpq defaults applied).
pub fn user_name(cfg: &DatabaseConfig) -> anyhow::Result<String> {
    Ok(base_options(cfg)?.get_username().to_owned())
}

pub fn connect_options(cfg: &DatabaseConfig) -> anyhow::Result<PgConnectOptions> {
    let mut opts = base_options(cfg)?;

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
    let opts = PgPoolOptions::new()
        .max_connections(cfg.pool_max)
        .min_connections(0)
        // Fail fast instead of hanging requests (and /readyz) when the database is unreachable.
        .acquire_timeout(cfg.connect_timeout)
        .after_release(|conn, _| Box::pin(close_if_in_transaction(conn)));
    if cfg.roles == RoleNames::default() {
        return opts;
    }
    let roles = cfg.roles.clone();
    opts.after_connect(move |conn, _| {
        let roles = roles.clone();
        Box::pin(async move { set_role_names(conn, &roles).await })
    })
}

/// The pool's release check (SHAA-2507). A request cancelled while sqlx's
/// `begin()` waits for the server (the client went away mid-request) leaves
/// the `BEGIN` applied but uncounted: sqlx 0.9's rollback guard only undoes a
/// counted transaction, so the connection went back to the pool inside an open
/// transaction. Its next user then ran in that transaction: `now()` stood still
/// (fresh CIs fell outside `ACTIVE_SQL`), autocommit writes stayed uncommitted
/// and `SET TRANSACTION` failed with a 500. Such a connection is closed, which
/// ends the transaction, and the pool opens a new one when needed.
///
/// `now()` is the transaction's start and equals `statement_timestamp()` only
/// in a statement's own implicit transaction. The simple protocol keeps both
/// on one message. sqlx runs the check in the task that returns the connection,
/// off the request's path.
async fn close_if_in_transaction(conn: &mut sqlx::PgConnection) -> sqlx::Result<bool> {
    use sqlx::Row;
    let open: bool = sqlx::raw_sql("SELECT now() <> statement_timestamp()").fetch_one(&mut *conn).await?.try_get(0)?;
    if open {
        tracing::info!("closed a database connection a cancelled request left inside a transaction");
    }
    Ok(!open)
}

/// For the schema owner's sessions: migrations and [`act_as_api_role`] read the role names from here.
pub async fn set_role_names(conn: &mut sqlx::PgConnection, roles: &RoleNames) -> sqlx::Result<()> {
    sqlx::query(
        "SELECT set_config('shadoucmdb.app_role', $1, false),
                set_config('shadoucmdb.maintenance_role', $2, false)",
    )
    .bind(roles.app.as_deref().unwrap_or_default())
    .bind(roles.maintenance.as_deref().unwrap_or_default())
    .execute(conn)
    .await?;
    Ok(())
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

pub async fn migrate(
    cfg: &DatabaseConfig,
    encryption: &crate::config::EncryptionConfig,
    adopt_drizzle: bool,
) -> anyhow::Result<()> {
    let pool = connect(cfg).await?;
    let mut result = migrate_with(&pool, cfg, adopt_drizzle).await;
    if result.is_ok() {
        // A missing or wrong ENCRYPTION_KEY_FILE shows here, before `serve` refuses to start.
        println!();
        result = async { crate::secrets::cli::report(&mut *pool.acquire().await?, encryption).await.map(|_| ()) }.await;
    }
    pool.close().await;
    result
}

/// Three-role install: switches the transaction to the API role, which owns
/// the area schemas and type tables and alters them at run time (migration
/// 0008 checked the membership). The role is `shadoucmdb.app_role` (see
/// [`RoleNames`]), else `shadoucmdb_app`. Returns whether it switched;
/// `RESET ROLE` switches back.
pub async fn act_as_api_role(conn: &mut sqlx::PgConnection) -> sqlx::Result<bool> {
    let api_role: Option<String> = sqlx::query_scalar(
        "SELECT rolname::text FROM pg_roles
         WHERE rolname = COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app')
           AND rolname <> current_user AND pg_has_role(current_user, oid, 'MEMBER')",
    )
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(role) = &api_role {
        // SET LOCAL ROLE, with the name as a parameter.
        sqlx::query("SELECT set_config('role', $1, true)").bind(role).execute(&mut *conn).await?;
    }
    Ok(api_role.is_some())
}

/// The only functions a default, check or domain expression on the API role's
/// objects may call: the DDL engine's enum check is `col = ANY ('{…}'::text[])`.
pub const PLANTED_CODE_ALLOWED_FUNCTIONS: &[&str] = &["pg_catalog.texteq(text,text)"];

/// Code the API role could have put where a migration runs it with the schema
/// owner's rights (GH#416): on a three-role install the API role owns the area
/// schemas and type tables, and migrations write to them (0009). ShadouCMDB
/// creates none of these objects there, so any one found is refused:
/// triggers, rules other than a view's `_RETURN`, row-level security and
/// policies on tables in the API role's schemas or owned by it, and functions
/// in those schemas or owned by it. Column defaults, check constraints and
/// domains run built-in functions too (GH#464), so their expressions may call
/// only [`PLANTED_CODE_ALLOWED_FUNCTIONS`] (the engine's enum checks). A
/// registered type table that is no longer a plain table, such as a view put
/// in its place, is refused too (GH#469). The engine's own views in those
/// schemas are the API role's code as well; this check does not cover them,
/// so a migration never reads them as the owner. A migration that reads or
/// writes any relation in an area schema first switches to the API role (`SET LOCAL ROLE`, as
/// [`act_as_api_role`] does; see `sql/README.md`), and this check is the
/// second line. `restore` and `factory-reset` check too, after dropping the
/// application's objects. Nothing to check on a single-role install, where the
/// API already runs as the owner.
pub async fn refuse_planted_code(conn: &mut sqlx::PgConnection) -> anyhow::Result<()> {
    let mut found: Vec<(String, String, String)> = sqlx::query_as(
        "WITH api AS (
           SELECT oid, rolname FROM pg_roles
           WHERE rolname = COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app')
             AND rolname <> current_user
         ),
         api_schema AS (SELECT n.oid FROM pg_namespace n JOIN api ON n.nspowner = api.oid),
         rel AS (
           SELECT c.oid, c.relrowsecurity, c.oid::regclass::text AS name FROM pg_class c, api
           WHERE c.relowner = api.oid OR c.relnamespace IN (SELECT oid FROM api_schema)
         ),
         dom AS (
           SELECT t.oid, t.typdefaultbin, t.oid::regtype::text AS name FROM pg_type t, api
           WHERE t.typtype = 'd' AND (t.typowner = api.oid OR t.typnamespace IN (SELECT oid FROM api_schema))
         ),
         expr AS (
           SELECT 'column default' AS kind, format('%s.%I', rel.name, a.attname) AS name, d.adbin::text AS bin
           FROM pg_attrdef d JOIN rel ON rel.oid = d.adrelid
           JOIN pg_attribute a ON a.attrelid = d.adrelid AND a.attnum = d.adnum
           UNION ALL
           SELECT 'check constraint', format('%I on %s', c.conname, rel.name), c.conbin::text
           FROM pg_constraint c JOIN rel ON rel.oid = c.conrelid WHERE c.contype = 'c'
           UNION ALL
           SELECT 'domain constraint', format('%I on %s', c.conname, dom.name), c.conbin::text
           FROM pg_constraint c JOIN dom ON dom.oid = c.contypid WHERE c.conbin IS NOT NULL
           UNION ALL
           SELECT 'domain default', dom.name, dom.typdefaultbin::text FROM dom WHERE dom.typdefaultbin IS NOT NULL
         ),
         -- Every function an expression calls: directly, or as an operator's
         -- implementation (an operator node may leave its function unresolved).
         called AS (
           SELECT expr.kind, expr.name, m[2]::oid AS fn FROM expr,
             regexp_matches(expr.bin, ':(funcid|opfuncid) ([0-9]+)', 'g') m WHERE m[2] <> '0'
           UNION ALL
           SELECT expr.kind, expr.name, o.oprcode::oid FROM expr,
             regexp_matches(expr.bin, ':opno ([0-9]+)', 'g') m JOIN pg_operator o ON o.oid = m[1]::oid
         )
         SELECT api.rolname::text, f.kind, f.name FROM api, (
           SELECT 'trigger' AS kind, format('%I on %s', t.tgname, rel.name) AS name
           FROM pg_trigger t JOIN rel ON rel.oid = t.tgrelid WHERE NOT t.tgisinternal
           UNION ALL
           SELECT 'rule', format('%I on %s', r.rulename, rel.name)
           FROM pg_rewrite r JOIN rel ON rel.oid = r.ev_class WHERE r.rulename <> '_RETURN'
           UNION ALL
           SELECT 'row-level security', rel.name FROM rel WHERE rel.relrowsecurity
           UNION ALL
           SELECT 'policy', format('%I on %s', p.polname, rel.name) FROM pg_policy p JOIN rel ON rel.oid = p.polrelid
           UNION ALL
           SELECT 'function', p.oid::regprocedure::text FROM pg_proc p, api
           WHERE p.proowner = api.oid OR p.pronamespace IN (SELECT oid FROM api_schema)
           UNION
           SELECT kind, name FROM called WHERE fn <> ALL ($1::regprocedure[]::oid[])
         ) f
         ORDER BY 2, 3",
    )
    .bind(PLANTED_CODE_ALLOWED_FUNCTIONS)
    .fetch_all(&mut *conn)
    .await?;
    // GH#469: the API role can drop a type table and create a view (or another
    // kind of relation) under its name; migrations name type tables by area
    // and class key, so they would read it as the owner. The relation's
    // comment proves nothing, the API role can set it. A type table not built
    // yet is not refused: nothing runs when it is missing. Before 0008 there is
    // no registry to check.
    let registry: bool = sqlx::query_scalar(
        "SELECT to_regclass('cmdb.ci_classes') IS NOT NULL AND to_regclass('cmdb.areas') IS NOT NULL",
    )
    .fetch_one(&mut *conn)
    .await?;
    if registry {
        found.extend(
            sqlx::query_as::<_, (String, String, String)>(
                "SELECT api.rolname::text,
                        CASE r.relkind WHEN 'v' THEN 'view' WHEN 'm' THEN 'materialized view'
                          WHEN 'f' THEN 'foreign table' WHEN 'p' THEN 'partitioned table'
                          ELSE 'relation' END || ' in place of the type table',
                        format('%I.%I', a.key, k.key)
                 FROM cmdb.ci_classes k JOIN cmdb.areas a ON a.id = k.area_id
                 JOIN pg_namespace n ON n.nspname = a.key
                 JOIN pg_class r ON r.relnamespace = n.oid AND r.relname = k.key,
                 pg_roles api
                 WHERE api.rolname = COALESCE(NULLIF(current_setting('shadoucmdb.app_role', true), ''), 'shadoucmdb_app')
                   AND api.rolname <> current_user AND r.relkind <> 'r'
                 ORDER BY 3",
            )
            .fetch_all(&mut *conn)
            .await?,
        );
    }
    let Some((role, _, _)) = found.first() else { return Ok(()) };
    let list: Vec<String> = found.iter().map(|(_, kind, name)| format!("  {kind} {name}")).collect();
    bail!(
        "refusing to change the schema: the API role \"{role}\" owns or can change these objects, and a migration \
         would run their code with the schema owner's rights. ShadouCMDB never creates them:\n{}\nFind out who \
         created them (they may have been planted through a compromised API role or DATABASE_URL), drop them as \
         the schema owner, and run the command again.",
        list.join("\n")
    );
}

/// GH#545: a database error inside a migration (a `RAISE EXCEPTION` stop such as
/// 0044's duplicate e-mails, or a failing statement) is reported once, with the
/// migration's name. sqlx repeats the message at every level of its error chain,
/// each with the line of the PostgreSQL source file that raised it, which means
/// nothing to an operator; DETAIL and HINT are kept.
fn migration_error(e: sqlx::migrate::MigrateError) -> anyhow::Error {
    let sqlx::migrate::MigrateError::ExecuteMigration(sqlx::Error::Database(db), version) = &e else {
        return anyhow::Error::new(e).context("migration failed");
    };
    let name = MIGRATOR.iter().find(|m| m.version == *version).map_or_else(|| version.to_string(), label);
    let mut msg = format!("migration {name} stopped: {}", db.message());
    if let Some(pg) = db.try_downcast_ref::<sqlx::postgres::PgDatabaseError>() {
        for (key, value) in [("DETAIL", pg.detail()), ("HINT", pg.hint())] {
            if let Some(value) = value {
                msg.push_str(&format!("\n{key}: {value}"));
            }
        }
    }
    anyhow::Error::msg(msg)
}

async fn migrate_with(pool: &PgPool, cfg: &DatabaseConfig, adopt_drizzle: bool) -> anyhow::Result<()> {
    let (db, version): (String, String) =
        sqlx::query_as("SELECT current_database(), current_setting('server_version')").fetch_one(pool).await?;
    println!("Connected to database \"{db}\" (PostgreSQL {version}), ssl={}", cfg.ssl.as_str());
    check_roles(pool, &cfg.roles).await?;
    refuse_planted_code(&mut *pool.acquire().await?).await?;

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

    // Nothing recorded yet (the table may not exist): nothing to rewrite.
    for &(version, old) in SUPERSEDED_CHECKSUMS.iter().filter(|_| !applied.is_empty()) {
        let Some(current) = MIGRATOR.iter().find(|m| m.version == version) else { continue };
        let updated = sqlx::query(
            "UPDATE public._sqlx_migrations SET checksum = $1 WHERE version = $2 AND checksum = decode($3, 'hex')",
        )
        .bind(&*current.checksum)
        .bind(version)
        .bind(old)
        .execute(pool)
        .await?
        .rows_affected();
        if updated > 0 {
            println!("  recorded the current text of {} (applied by a pre-release build)", label(current));
        }
    }

    // The accounts table comes with 0003: nothing to check on an empty database.
    if applied.contains(&3) && pending.iter().any(|m| m.version == 44) {
        refuse_accounts_person_refuses(pool).await?;
    }

    // Each pending migration runs in its own transaction together with its
    // bookkeeping row, under an advisory lock; re-running is a no-op.
    MIGRATOR.run(pool).await.map_err(|e| {
        let denied = e.to_string().contains("permission denied");
        let err = migration_error(e);
        if denied {
            // The usual cause on a three-role install: migrating with the API's DATABASE_URL.
            err.context("this database user may not change the schema; set MIGRATION_DATABASE_URL to the schema owner role (shadoucmdb_owner in sql/bootstrap/)")
        } else {
            err
        }
    })?;

    for m in &pending {
        println!("  applied {}", label(m));
    }
    let after = applied_count(pool).await?;

    let (change, links) = reconcile_and_link(pool).await?;
    if links.linked > 0 {
        println!(
            "Users: {} accounts linked to their Person (created where none had the account's e-mail)",
            links.linked
        );
    }
    if !links.refused.is_empty() {
        println!(
            "Warning: {} accounts could not be linked to a Person; they cannot sign in, and their API tokens are \
             refused, until they are (GET /api/v1/admin/users?signInStatus=person_missing lists them). Correct each \
             account's e-mail or display name (Administration > Users), then run `shadoucmdb migrate` again; the \
             other accounts are linked. If your own administrator account is listed, create a recovery administrator with \
             `shadoucmdb create-admin`:",
            links.refused.len()
        );
        for r in &links.refused {
            println!("  {r}");
        }
    }
    let waiting = crate::data::auth::count_without_email(&mut *pool.acquire().await?).await?;
    if waiting > 0 {
        println!(
            "Users: {waiting} accounts have no e-mail yet; they must enter one at their next sign-in (listed under \
             Administration > Users, sign-in status \"e-mail required\")"
        );
    }
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
    if let Some(notice) = crate::data::api_tokens::second_factor_refusal_notice(&mut *pool.acquire().await?).await? {
        println!("Warning: {notice}");
    }
    if let Some(notice) = crate::data::api_tokens::email_required_refusal_notice(&mut *pool.acquire().await?).await? {
        println!("Warning: {notice}");
    }
    println!(
        "Database is at migration {after}/{expected}{}",
        if pending.is_empty() && !reconciled { " (nothing to do)" } else { "" }
    );
    Ok(())
}

/// What `migrate` does after the migrations: area schemas, type tables and
/// reporting views follow the data model (anything missing after migration
/// 0009, a new reporting role, the Person type of 0044), then every account
/// with an e-mail gets its Person (SHAA-1505), in the same transaction as the
/// reconcile that built its table. An account the Person type refuses stays
/// unlinked and is reported, the others are linked (GH#543). Returns the
/// schema change and what the linking did.
pub async fn reconcile_and_link(
    pool: &PgPool,
) -> anyhow::Result<(Option<crate::schema::SchemaChange>, crate::modules::people::LinkReport)> {
    let ctx = crate::api::context::RequestContext::system("migrate", "migrate");
    let mut tx = pool.begin().await?;
    act_as_api_role(&mut tx).await?;
    let change = crate::schema::reconcile(&mut tx, &ctx, "Reconcile after migrate")
        .await
        .map_err(|e| anyhow::anyhow!("reconciling the data model failed: {}", e.message))?;
    let links = crate::modules::people::link_all(&mut tx, &ctx)
        .await
        .map_err(|e| anyhow::anyhow!("linking the user accounts to their persons failed: {}", e.message))?;
    tx.commit().await?;
    Ok((change, links))
}

/// Before migration 0044 (GH#543): accounts whose e-mail or display name the
/// new Person type would refuse stop the upgrade with the list, before
/// anything changes. Once 0044 is applied, linking such an account fails and
/// it cannot sign in.
pub(crate) async fn refuse_accounts_person_refuses(pool: &PgPool) -> anyhow::Result<()> {
    // Unqualified: before migration 0008 (v0.1.0-rc.1) the table is in `public`,
    // and the search_path finds it in either schema.
    let accounts: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT username, email, display_name FROM users WHERE email IS NOT NULL ORDER BY lower(username)",
    )
    .fetch_all(pool)
    .await?;
    let refused: Vec<String> = accounts
        .iter()
        .filter_map(|(username, email, name)| {
            crate::modules::people::person_refuses(email, name).map(|why| format!("  {username}: {why}"))
        })
        .collect();
    if refused.is_empty() {
        return Ok(());
    }
    bail!(
        "migration 0044 links every account with an e-mail to a Person, whose Email holds at most {} characters \
         and whose Name (the display name) at most {}, and these accounts do not fit:\n{}\nGive each account a \
         shorter e-mail or display name (Administration > Users) with the previous release, then run \
         `shadoucmdb migrate` again. Nothing was changed.",
        crate::modules::people::PERSON_EMAIL_MAX,
        crate::modules::people::PERSON_NAME_MAX,
        refused.join("\n")
    );
}

/// One-time hand-over from the Node/Drizzle runner: checks that every row in
/// The API and maintenance roles the migrations grant to must exist; a typo in
/// a user name would otherwise leave them without privileges, silently.
async fn check_roles(pool: &PgPool, roles: &RoleNames) -> anyhow::Result<()> {
    let owner: String = sqlx::query_scalar("SELECT current_user::text").fetch_one(pool).await?;
    for (var, purpose, role) in
        [("DATABASE_URL", "API", &roles.app), ("MAINTENANCE_DATABASE_URL", "maintenance", &roles.maintenance)]
    {
        let Some(role) = role else { continue };
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT FROM pg_roles WHERE rolname = $1)")
            .bind(role)
            .fetch_one(pool)
            .await?;
        if !exists {
            bail!(
                "the {purpose} role \"{role}\" (the {var} user) does not exist on this server; create it \
                 (sql/bootstrap/) or correct {var}"
            );
        }
        println!(
            "{purpose} role: {role}{}",
            if *role == owner { " (the migrating user; single-role install)" } else { "" }
        );
    }
    Ok(())
}

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

    /// A restore, factory reset or decommission rebuilds or drops the whole
    /// schema in one transaction and holds a lock on every object it touches
    /// (some 4,400 with an empty data model). PostgreSQL's lock table is shared
    /// by the whole server and sized by `max_locks_per_transaction` (64 by
    /// default): a few such transactions at once fail with `53200 out of
    /// shared memory` on a many-core host (GH#647). Tests hold this permit
    /// around them, so one runs at a time and the default setting suffices.
    pub async fn whole_schema_transaction() -> tokio::sync::SemaphorePermit<'static> {
        static ONE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
        ONE.acquire().await.expect("never closed")
    }

    /// A database `shadoucmdb migrate` left: migrated and reconciled.
    pub async fn database(test: &str) -> Option<Scratch> {
        let db = empty(test).await?;
        super::MIGRATOR.run(&db.pool).await.expect("migrations");
        super::reconcile_and_link(&db.pool).await.expect("reconcile after migrate");
        Some(db)
    }

    /// A database with no migrations applied, as after `CREATE DATABASE`.
    pub async fn empty(test: &str) -> Option<Scratch> {
        let admin = admin(test)?;
        Some(create(admin, &[]).await)
    }

    fn admin(test: &str) -> Option<PgConnectOptions> {
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
        Some(PgConnectOptions::from_str(&url).expect("SHADOUCMDB_TEST_DATABASE_URL"))
    }

    async fn create(admin: PgConnectOptions, settings: &[(&str, &str)]) -> Scratch {
        let name = format!("shadoucmdb_test_{}", uuid::Uuid::new_v4().simple());
        let mut c = admin.connect().await.expect("connect to SHADOUCMDB_TEST_DATABASE_URL");
        c.execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}"))).await.expect("CREATE DATABASE");
        c.close().await.ok();
        // The same search_path as the application's pool (system tables live in `cmdb`).
        let opts = admin
            .clone()
            .database(&name)
            .options([("search_path", super::SEARCH_PATH)])
            .options(settings.iter().copied());
        let pool = PgPoolOptions::new().max_connections(8).connect_with(opts).await.unwrap();
        Scratch { admin, name, pool }
    }

    /// A three-role install of a test's own: an API and a maintenance role
    /// (NOLOGIN) named for this test, which its databases name in
    /// `shadoucmdb.app_role` / `shadoucmdb.maintenance_role` as
    /// [`super::RoleNames`] does. Roles are cluster-wide, so no test creates
    /// the default `shadoucmdb_app`: once it exists, every other test's
    /// restore switches to it (`act_as_api_role`), and a database migrated
    /// before then granted it nothing (GH#421).
    pub struct Roles {
        admin: PgConnectOptions,
        pub app: String,
        pub maintenance: String,
    }

    impl Roles {
        pub async fn create(test: &str) -> Option<Roles> {
            let admin = admin(test)?;
            let id = uuid::Uuid::new_v4().simple().to_string();
            let roles = Roles {
                app: format!("shadoucmdb_test_{}_app", &id[..12]),
                maintenance: format!("shadoucmdb_test_{}_maint", &id[..12]),
                admin,
            };
            let mut c = roles.admin.connect().await.expect("connect to SHADOUCMDB_TEST_DATABASE_URL");
            for role in [&roles.app, &roles.maintenance] {
                c.execute(sqlx::AssertSqlSafe(format!("CREATE ROLE {role} NOLOGIN"))).await.unwrap();
            }
            c.close().await.ok();
            Some(roles)
        }

        /// A database migrated as its owner, with the grants to these roles.
        pub async fn database(&self) -> Scratch {
            let db = self.empty().await;
            super::MIGRATOR.run(&db.pool).await.expect("migrations");
            super::reconcile_and_link(&db.pool).await.expect("reconcile after migrate");
            db
        }

        /// A database of these roles with no migrations applied, for upgrade tests.
        pub async fn empty(&self) -> Scratch {
            let settings = [
                ("shadoucmdb.app_role", self.app.as_str()),
                ("shadoucmdb.maintenance_role", self.maintenance.as_str()),
            ];
            create(self.admin.clone(), &settings).await
        }

        /// A pool on `db` that works as the API role, as the running application does.
        pub async fn api_pool(&self, db: &Scratch) -> PgPool {
            let opts = (*db.pool.connect_options()).clone().options([("role", self.app.as_str())]);
            PgPoolOptions::new().max_connections(4).connect_with(opts).await.unwrap()
        }

        /// After every database of these roles is dropped.
        pub async fn drop(self) {
            let mut c = self.admin.connect().await.unwrap();
            for role in [&self.app, &self.maintenance] {
                c.execute(sqlx::AssertSqlSafe(format!("DROP ROLE {role}"))).await.unwrap();
            }
        }
    }

    impl Scratch {
        pub fn name(&self) -> &str {
            &self.name
        }

        /// `WITH (FORCE)` signals the database's other sessions and waits
        /// five seconds for them to exit; on a loaded host a session can take
        /// longer and the drop fails with `55006 object_in_use` (GH#721). New
        /// connections are refused first, then the drop is retried.
        pub async fn drop(self) {
            self.pool.close().await;
            let mut c = self.admin.connect().await.unwrap();
            let refuse = format!("ALTER DATABASE {} WITH ALLOW_CONNECTIONS false", self.name);
            c.execute(sqlx::AssertSqlSafe(refuse)).await.ok();
            let drop = format!("DROP DATABASE {} WITH (FORCE)", self.name);
            for attempt in 1.. {
                match c.execute(sqlx::AssertSqlSafe(drop.clone())).await {
                    Ok(_) => return,
                    Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("55006") && attempt < 12 => {
                        tokio::time::sleep(std::time::Duration::from_millis(250 * attempt)).await;
                    }
                    Err(e) => panic!("DROP DATABASE {}: {e}", self.name),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MIGRATOR, SUPERSEDED_CHECKSUMS, checksum_matches, is_loopback_host};

    #[test]
    fn superseded_checksums_are_older_texts_of_shipped_migrations() {
        for &(version, old) in SUPERSEDED_CHECKSUMS {
            let m = MIGRATOR.iter().find(|m| m.version == version).expect("a shipped migration");
            assert_ne!(hex::encode(&*m.checksum), old, "migration {version} still has this text");
            assert!(checksum_matches(version, old));
            assert!(checksum_matches(version, &hex::encode(&*m.checksum)));
            assert!(!checksum_matches(version + 100, old));
        }
    }

    /// `shadoucmdb migrate` on an empty database, as on a first install.
    #[tokio::test]
    async fn migrate_runs_on_an_empty_database_and_again_as_a_no_op() {
        let Some(db) = super::scratch::empty("migrate_runs_on_an_empty_database").await else { return };
        let cfg = crate::config::DatabaseConfig {
            url: None,
            host: None,
            port: 5432,
            database: None,
            user: None,
            password: None,
            ssl: crate::config::SslMode::Disable,
            ssl_ca_file: None,
            pool_max: 1,
            statement_timeout: std::time::Duration::ZERO,
            connect_timeout: std::time::Duration::from_secs(1),
            roles: Default::default(),
        };
        super::migrate_with(&db.pool, &cfg, false).await.expect("first migrate");
        super::migrate_with(&db.pool, &cfg, false).await.expect("second migrate");
        assert_eq!(super::applied_count(&db.pool).await.unwrap(), super::expected_count());
        db.drop().await;
    }

    /// SHAA-2507: a `begin()` cancelled after its `BEGIN` reached the server
    /// leaves the transaction open. Without the release check the next user of
    /// the pool's one connection ran inside it (frozen `now()`, uncommitted
    /// writes); with it, that connection is closed and the next one is clean.
    #[tokio::test]
    async fn a_cancelled_begin_does_not_leak_its_transaction_into_the_pool() {
        use sqlx::Connection;
        use std::future::Future;
        let Some(db) = super::scratch::empty("a_cancelled_begin_does_not_leak").await else { return };
        let opts = (*db.pool.connect_options()).clone();
        let cfg = crate::config::DatabaseConfig {
            url: None,
            host: None,
            port: 5432,
            database: None,
            user: None,
            password: None,
            ssl: crate::config::SslMode::Disable,
            ssl_ca_file: None,
            pool_max: 1,
            statement_timeout: std::time::Duration::ZERO,
            connect_timeout: std::time::Duration::from_secs(5),
            roles: Default::default(),
        };
        let mut monitor = sqlx::PgConnection::connect_with(&opts).await.unwrap();
        let pool = super::pool_options(&cfg).connect_with(opts).await.unwrap();
        for _ in 0..3 {
            let mut c = pool.acquire().await.unwrap();
            {
                // One poll sends the statement; the server applies the BEGIN and
                // holds the reply back, so begin() is still waiting when dropped.
                let mut begin = std::pin::pin!((*c).begin_with("BEGIN; SELECT pg_sleep(0.3)"));
                let polled = std::future::poll_fn(|cx| std::task::Poll::Ready(begin.as_mut().poll(cx))).await;
                assert!(polled.is_pending(), "begin() waits for the server");
            }
            drop(c);
            // The pool's one connection, once released: idle, not idle in transaction.
            let mut next = pool.acquire().await.unwrap();
            let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *next).await.unwrap();
            let state: String = sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid = $1")
                .bind(pid)
                .fetch_one(&mut monitor)
                .await
                .unwrap();
            assert_eq!(state, "idle", "the connection the pool hands out next");
        }
        monitor.close().await.ok();
        pool.close().await;
        db.drop().await;
    }

    /// SHAA-2553 (#771): the release check closes only connections left inside
    /// a transaction. One used for plain statements or a finished transaction
    /// goes back to the pool and is handed out again (same backend), so the
    /// check does not turn every request into a new connection. A transaction
    /// opened by a raw `BEGIN`, not through `begin()`, is closed as well, and
    /// its uncommitted write is gone.
    #[tokio::test]
    async fn the_release_check_keeps_clean_connections_and_closes_open_transactions() {
        use sqlx::Connection;
        let Some(db) = super::scratch::empty("the_release_check_keeps_clean").await else { return };
        let opts = (*db.pool.connect_options()).clone();
        let cfg = crate::config::DatabaseConfig {
            url: None,
            host: None,
            port: 5432,
            database: None,
            user: None,
            password: None,
            ssl: crate::config::SslMode::Disable,
            ssl_ca_file: None,
            pool_max: 1,
            statement_timeout: std::time::Duration::ZERO,
            connect_timeout: std::time::Duration::from_secs(5),
            roles: Default::default(),
        };
        let pool = super::pool_options(&cfg).connect_with(opts).await.unwrap();
        sqlx::raw_sql("CREATE TABLE release_check (n int)").execute(&pool).await.unwrap();
        async fn pid(c: &mut sqlx::PgConnection) -> i32 {
            sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(c).await.unwrap()
        }

        let mut c = pool.acquire().await.unwrap();
        let first = pid(&mut c).await;
        drop(c);
        let mut c = pool.acquire().await.unwrap();
        assert_eq!(pid(&mut c).await, first, "a connection used outside a transaction is kept");
        let mut tx = (*c).begin().await.unwrap();
        sqlx::query("INSERT INTO release_check VALUES (1)").execute(&mut *tx).await.unwrap();
        tx.commit().await.unwrap();
        drop(c);
        let mut c = pool.acquire().await.unwrap();
        assert_eq!(pid(&mut c).await, first, "a connection whose transaction committed is kept");
        let tx = (*c).begin().await.unwrap();
        drop(tx);
        drop(c);
        let mut c = pool.acquire().await.unwrap();
        assert_eq!(pid(&mut c).await, first, "a connection whose transaction rolled back is kept");

        sqlx::raw_sql("BEGIN; INSERT INTO release_check VALUES (2)").execute(&mut *c).await.unwrap();
        drop(c);
        let mut c = pool.acquire().await.unwrap();
        assert_ne!(pid(&mut c).await, first, "a connection left inside a transaction is replaced");
        let rows: Vec<i32> =
            sqlx::query_scalar("SELECT n FROM release_check ORDER BY n").fetch_all(&mut *c).await.unwrap();
        assert_eq!(rows, [1], "the open transaction's write was not committed");
        drop(c);
        pool.close().await;
        db.drop().await;
    }

    /// GH#545: a migration's `RAISE EXCEPTION` stop prints its message once,
    /// without sqlx's chain and the `at line N` of the PostgreSQL source.
    #[tokio::test]
    async fn a_migration_stop_reports_the_database_message_once() {
        let Some(db) = super::scratch::empty("a_migration_stop_reports_once").await else { return };
        let err = sqlx::query(
            "DO $$ BEGIN RAISE EXCEPTION 'users: e-mail addresses must be unique: %', 'alice@acme.test'
               USING ERRCODE = 'unique_violation', HINT = 'Give each account its own address.'; END $$",
        )
        .execute(&db.pool)
        .await
        .unwrap_err();
        let e = super::migration_error(sqlx::migrate::MigrateError::ExecuteMigration(err, 44));
        assert_eq!(
            format!("{e:#}"),
            "migration 0044_users_person stopped: users: e-mail addresses must be unique: alice@acme.test\n\
             HINT: Give each account its own address."
        );
        db.drop().await;
    }

    /// GH#416: a migrated three-role database with a data model passes; code
    /// planted where the API role can put it stops `migrate`.
    #[tokio::test]
    async fn migrate_refuses_code_planted_in_the_api_roles_objects() {
        use sqlx::{Connection, Executor};
        let Some(roles) = super::scratch::Roles::create("migrate_refuses_code_planted").await else { return };
        let db = roles.database().await;
        let mut tx = db.pool.begin().await.unwrap();
        assert!(super::act_as_api_role(&mut tx).await.unwrap(), "three-role install");
        let ctx = crate::api::context::RequestContext::system("test", "test");
        let template = crate::modules::templates::find("it_infrastructure").unwrap();
        let installed = crate::modules::templates::install(&mut tx, &ctx, template).await.map_err(|e| e.message);
        assert!(installed.unwrap().created.classes > 0);
        tx.commit().await.unwrap();
        let mut c = db.pool.acquire().await.unwrap();
        let api_schemas: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_namespace WHERE nspowner = (SELECT oid FROM pg_roles WHERE rolname = $1)",
        )
        .bind(&roles.app)
        .fetch_one(&mut *c)
        .await
        .unwrap();
        assert!(api_schemas > 0, "the API role owns the area schemas");
        let enum_checks: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_constraint c JOIN pg_class t ON t.oid = c.conrelid
             WHERE c.contype = 'c' AND t.relowner = (SELECT oid FROM pg_roles WHERE rolname = $1)",
        )
        .bind(&roles.app)
        .fetch_one(&mut *c)
        .await
        .unwrap();
        assert!(enum_checks > 0, "the template's enum fields have checks, and they pass");
        super::refuse_planted_code(&mut c).await.expect("ShadouCMDB itself creates none of these objects");

        let table: String = sqlx::query_scalar(
            "SELECT c.oid::regclass::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE c.relkind = 'r' AND n.nspowner = (SELECT oid FROM pg_roles WHERE rolname = $1)
             ORDER BY 1 LIMIT 1",
        )
        .bind(&roles.app)
        .fetch_one(&mut *c)
        .await
        .unwrap();
        let schema = table.split('.').next().unwrap().to_owned();
        let mut tx = c.begin().await.unwrap();
        for sql in [
            format!(
                "CREATE FUNCTION {schema}.planted() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$"
            ),
            format!("CREATE TRIGGER planted BEFORE INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION {schema}.planted()"),
            format!("CREATE RULE planted AS ON DELETE TO {table} DO ALSO NOTIFY planted"),
            format!("ALTER TABLE {table} ENABLE ROW LEVEL SECURITY"),
            format!("CREATE POLICY planted ON {table} USING (true)"),
            "CREATE FUNCTION public.planted_owned() RETURNS int LANGUAGE sql AS 'SELECT 1'".to_owned(),
            format!("ALTER FUNCTION public.planted_owned() OWNER TO {}", roles.app),
            // GH#464: expressions that run built-ins as whoever writes the row.
            format!(
                "ALTER TABLE {table} ADD COLUMN planted_default text DEFAULT query_to_xml('SELECT 1', true, true, '')::text"
            ),
            format!("ALTER TABLE {table} ADD COLUMN planted_constant text DEFAULT 'harmless'"),
            format!("ALTER TABLE {table} ADD CONSTRAINT planted_check CHECK (set_config('a.b', 'c', true) <> '')"),
            "CREATE OPERATOR public.=== (FUNCTION = pg_catalog.pg_notify, LEFTARG = text, RIGHTARG = text)".to_owned(),
            format!("ALTER OPERATOR public.=== (text, text) OWNER TO {}", roles.app),
            format!("ALTER TABLE {table} ADD CONSTRAINT planted_operator CHECK ((id::text === 'x') IS NULL)"),
            format!("CREATE DOMAIN {schema}.planted_domain AS text DEFAULT now()::text CHECK (VALUE ~ 'x')"),
        ] {
            tx.execute(sqlx::AssertSqlSafe(sql)).await.unwrap();
        }
        let err = super::refuse_planted_code(&mut tx).await.unwrap_err().to_string();
        tx.rollback().await.unwrap();
        for expected in [
            format!("the API role \"{}\"", roles.app),
            format!("  function {schema}.planted()"),
            "  function planted_owned()".to_owned(),
            format!("  trigger planted on {table}"),
            format!("  rule planted on {table}"),
            format!("  row-level security {table}"),
            format!("  policy planted on {table}"),
            format!("  column default {table}.planted_default"),
            format!("  check constraint planted_check on {table}"),
            format!("  check constraint planted_operator on {table}"),
            format!("  domain default {schema}.planted_domain"),
            format!("  domain constraint planted_domain_check on {schema}.planted_domain"),
        ] {
            assert!(err.contains(&expected), "{expected:?} missing from: {err}");
        }
        assert!(!err.contains("planted_constant"), "a constant default calls nothing: {err}");
        assert!(!err.contains("constraint ck_"), "the engine's enum checks pass: {err}");

        // GH#469: a view put in place of a type table runs its functions as
        // whoever reads it, e.g. migration 0036 as the owner.
        let application: String =
            sqlx::query_scalar("SELECT cmdb.type_table(id) FROM cmdb.ci_classes WHERE key = 'application'")
                .fetch_one(&mut *c)
                .await
                .unwrap();
        let mut tx = c.begin().await.unwrap();
        tx.execute(sqlx::AssertSqlSafe(format!("DROP TABLE {application} CASCADE"))).await.unwrap();
        tx.execute(sqlx::AssertSqlSafe(format!(
            "CREATE VIEW {application} AS SELECT gen_random_uuid() AS id, set_config('search_path', 'x', false) AS criticality"
        )))
        .await
        .unwrap();
        let err = super::refuse_planted_code(&mut tx).await.unwrap_err().to_string();
        tx.rollback().await.unwrap();
        let expected = format!("  view in place of the type table {application}");
        assert!(err.contains(&expected), "{expected:?} missing from: {err}");
        drop(c);
        db.drop().await;
        roles.drop().await;
    }

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
#[cfg(test)]
mod upgrade_0019;
#[cfg(test)]
mod upgrade_0020;
#[cfg(test)]
mod upgrade_0022;
#[cfg(test)]
mod upgrade_0023;
#[cfg(test)]
mod upgrade_0024;
#[cfg(test)]
pub(crate) mod upgrade_0029;
#[cfg(test)]
pub(crate) mod upgrade_0033;
#[cfg(test)]
mod upgrade_0036;
#[cfg(test)]
mod upgrade_0039;
#[cfg(test)]
mod upgrade_0041;
#[cfg(test)]
mod upgrade_0042;
#[cfg(test)]
mod upgrade_0044;
#[cfg(test)]
mod upgrade_0045;
#[cfg(test)]
pub(crate) mod upgrade_0046;
#[cfg(test)]
mod upgrade_0048;
#[cfg(test)]
pub(crate) mod upgrade_0051;
#[cfg(test)]
mod upgrade_0058;
#[cfg(test)]
mod upgrade_0061;
#[cfg(test)]
mod upgrade_0063;
#[cfg(test)]
mod upgrade_0064;
#[cfg(test)]
mod upgrade_0065;
#[cfg(test)]
mod upgrade_0070;
#[cfg(test)]
mod upgrade_0071;
#[cfg(test)]
mod upgrade_0072;
