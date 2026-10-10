//! `shadoucmdb`: the ShadouCMDB server and its admin commands in one binary.

mod api;
mod audit_export;
mod audit_verify;
mod auth;
mod config;
mod data;
mod db;
mod http;
mod logging;
mod maintenance;
mod modules;
mod prune;
mod schema;
mod secrets;
mod seed;
mod service;
mod verify;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};

use config::Config;

#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(name = "shadoucmdb", version, about = "ShadouCMDB server and admin commands")]
struct Cli {
    /// Load environment variables from this file (variables already set in the
    /// environment win). Without it, only .env in the current directory is
    /// loaded, if it exists; parent directories are never searched.
    #[arg(long, global = true, value_name = "PATH")]
    env_file: Option<PathBuf>,

    /// Append logs to this file instead of writing them to stdout.
    #[arg(long, global = true, value_name = "PATH")]
    log_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the HTTP server (API, health checks and the embedded web UI).
    Serve,
    /// Apply pending database migrations. Safe to re-run.
    Migrate {
        /// One-time hand-over for a database migrated by the old Node/Drizzle runner.
        #[arg(long)]
        adopt_drizzle: bool,
    },
    /// Check the system rows; optionally install a starter template or a demo inventory (idempotent).
    Seed {
        /// Install this starter template (repeatable), e.g. it_infrastructure.
        #[arg(long = "template", value_name = "KEY")]
        templates: Vec<String>,
        /// Install the it_infrastructure template and load a small demo inventory into a database without CIs.
        #[arg(long)]
        demo: bool,
    },
    /// Run schema acceptance checks inside a rolled-back transaction.
    Verify,
    /// Check the audit_log hash chain; prints the chain head to compare with the SIEM copy.
    AuditVerify {
        /// Accept gaps in chainSeq that a prune-audit run (audit.purge entry) accounts for. Other gaps always fail.
        #[arg(long)]
        allow_gaps: bool,
    },
    /// Create a user with the built-in Administrator profile (first install or lost access).
    CreateAdmin(auth::cli::CreateAdminArgs),
    /// Delete audit_log rows past the retention window (a dry run unless --execute).
    PruneAudit(prune::PruneAuditArgs),
    /// Write a consistent backup of all data and settings to a file (checked after writing).
    Backup(maintenance::backup::BackupArgs),
    /// Check a backup file and restore it, all or nothing.
    Restore(maintenance::restore::RestoreArgs),
    /// Delete all data, users and settings and start again at first-run setup.
    FactoryReset(maintenance::ConfirmArgs),
    /// Remove every ShadouCMDB table, row and setting from the database before retiring it.
    Decommission(maintenance::ConfirmArgs),
    /// Create a new key for ENCRYPTION_KEY_FILE (offline; never overwrites a file).
    GenerateEncryptionKey(secrets::cli::GenerateKeyArgs),
    /// Two-factor authentication maintenance.
    #[command(subcommand)]
    Mfa(secrets::cli::MfaCommand),
    /// Identity provider (OIDC, LDAP) maintenance.
    #[command(subcommand)]
    IdentityProviders(secrets::cli::IdentityProvidersCommand),
    /// Webhook endpoint maintenance.
    #[command(subcommand)]
    Webhooks(secrets::cli::WebhooksCommand),
    /// Print the OpenAPI document generated from the code, or compare it with a file.
    Openapi {
        /// Write the document to this file instead of stdout.
        #[arg(long, value_name = "PATH", conflicts_with = "check")]
        out: Option<PathBuf>,
        /// Fail if this file differs from the generated document (CI: the committed spec is stale).
        #[arg(long, value_name = "PATH")]
        check: Option<PathBuf>,
    },
    /// Install, remove or run as a Windows Service.
    #[command(subcommand)]
    Service(service::ServiceCommand),
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Serve => "serve",
            Command::Migrate { .. } => "migrate",
            Command::Seed { .. } => "seed",
            Command::Verify => "verify",
            Command::AuditVerify { .. } => "audit-verify",
            Command::CreateAdmin(_) => "create-admin",
            Command::PruneAudit(_) => "prune-audit",
            Command::Backup(_) => "backup",
            Command::Restore(_) => "restore",
            Command::FactoryReset(_) => "factory-reset",
            Command::Decommission(_) => "decommission",
            Command::GenerateEncryptionKey(_) => "generate-encryption-key",
            Command::Mfa(_) => "mfa",
            Command::IdentityProviders(_) => "identity-providers",
            Command::Webhooks(_) => "webhooks",
            Command::Openapi { .. } => "openapi",
            Command::Service(_) => "service",
        }
    }
}

/// Loads `--env-file`, or else `.env` in the working directory. Returns the
/// file loaded, as an absolute path, for the startup log.
fn load_env_file(path: Option<&PathBuf>) -> anyhow::Result<Option<PathBuf>> {
    let loaded = match path {
        Some(p) => {
            dotenvy::from_path(p).with_context(|| format!("cannot read env file {}", p.display()))?;
            Some(p.clone())
        }
        None => load_dot_env_in(&std::env::current_dir().context("cannot determine the working directory")?)?,
    };
    let Some(p) = loaded else { return Ok(None) };
    let p = std::path::absolute(&p).unwrap_or(p);
    config::set_env_file(p.clone());
    Ok(Some(p))
}

/// Loads `<dir>/.env` if it exists. Never looks in parent directories
/// (`dotenvy::dotenv()` does): a command started in a subdirectory must not
/// pick up another installation's database or write its setup token there.
fn load_dot_env_in(dir: &std::path::Path) -> anyhow::Result<Option<PathBuf>> {
    let p = dir.join(".env");
    match dotenvy::from_path(&p) {
        Ok(()) => Ok(Some(p)),
        Err(e) if e.not_found() => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", p.display())),
    }
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().enable_all().build()?)
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let env_file = load_env_file(cli.env_file.as_ref())?;
    let launch = service::LaunchOptions { env_file: cli.env_file, log_file: cli.log_file.clone() };

    // Generating the spec needs no configuration, database or logger.
    if let Command::Openapi { out, check } = &cli.command {
        return openapi(out.as_deref(), check.as_deref());
    }

    // Nor does making a key, which works offline.
    if let Command::GenerateEncryptionKey(args) = cli.command {
        return secrets::cli::generate_key(args);
    }

    // Installing or removing a service needs neither configuration nor a logger.
    if let Command::Service(
        cmd @ (service::ServiceCommand::Install { .. } | service::ServiceCommand::Uninstall { .. }),
    ) = cli.command
    {
        return service::run(cmd, launch);
    }

    // Read LOG_LEVEL leniently here so that a broken configuration is still
    // reported through the logger; Config::from_env validates it properly.
    logging::init(&std::env::var("LOG_LEVEL").unwrap_or_default(), cli.log_file.as_deref())?;
    // The path only, never the values: it shows which installation's settings apply.
    if let Some(p) = &env_file {
        tracing::info!("loaded environment from {}", p.display());
    }

    match cli.command {
        Command::Serve => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(http::serve(cfg, http::shutdown_signal()))
        }
        Command::Migrate { adopt_drizzle } => {
            let cfg = Config::from_env()?;
            let encryption = cfg.encryption.clone();
            let db = cfg.schema_owner_database()?;
            runtime()?.block_on(db::migrate(&db, &encryption, adopt_drizzle))
        }
        Command::Seed { templates, demo } => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(seed::run(&cfg.database, &templates, demo))
        }
        Command::Verify => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(verify::run(&cfg.database, &cfg.encryption))
        }
        Command::AuditVerify { allow_gaps } => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(audit_verify::run(&cfg.database, allow_gaps))
        }
        Command::CreateAdmin(args) => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(auth::cli::create_admin(&cfg.database, args))
        }
        Command::PruneAudit(args) => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(prune::run(&cfg, args))
        }
        Command::Backup(args) => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(maintenance::backup::run(&cfg.database, &cfg.encryption, args))
        }
        // These rebuild or drop the schema: like `migrate`, they connect as the
        // schema owner when MIGRATION_DATABASE_URL is set.
        Command::Restore(args) => {
            let cfg = Config::from_env()?;
            let encryption = cfg.encryption.clone();
            let export = cfg.audit.export.clone();
            let db = cfg.schema_owner_database()?;
            runtime()?.block_on(maintenance::restore::run(&db, &encryption, export.as_ref(), args))
        }
        Command::FactoryReset(args) => {
            let db = Config::from_env()?.schema_owner_database()?;
            runtime()?.block_on(maintenance::reset::factory_reset_cmd(&db, args))
        }
        Command::Decommission(args) => {
            let db = Config::from_env()?.schema_owner_database()?;
            runtime()?.block_on(maintenance::reset::decommission_cmd(&db, args))
        }
        Command::Mfa(cmd) => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(secrets::cli::mfa(&cfg, cmd))
        }
        Command::IdentityProviders(cmd) => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(secrets::cli::identity_providers(&cfg, cmd))
        }
        Command::Webhooks(cmd) => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(secrets::cli::webhooks(&cfg, cmd))
        }
        Command::Service(cmd) => service::run(cmd, launch),
        Command::Openapi { .. } | Command::GenerateEncryptionKey(_) => unreachable!("handled above"),
    }
}

fn openapi(out: Option<&std::path::Path>, check: Option<&std::path::Path>) -> anyhow::Result<()> {
    let generated = api::openapi_json();
    if let Some(path) = check {
        let committed = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        if committed.replace("\r\n", "\n") != generated {
            anyhow::bail!(
                "{} is stale: it differs from the OpenAPI document generated from the code.\n\
                 Regenerate it with `shadoucmdb openapi --out {}` and commit the result.",
                path.display(),
                path.display()
            );
        }
        println!("{} is up to date", path.display());
        return Ok(());
    }
    match out {
        Some(path) => {
            std::fs::write(path, &generated).with_context(|| format!("cannot write {}", path.display()))?;
            println!("wrote {}", path.display());
        }
        None => print!("{generated}"),
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let command = cli.command.name();
    // The server reports through its structured log (which may be a file);
    // the admin commands talk to a human on stderr.
    let long_running = matches!(cli.command, Command::Serve | Command::Service(_));
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) if long_running && tracing::dispatcher::has_been_set() => {
            tracing::error!(error = %format!("{err:#}"), "{command} failed");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("{command} failed: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // GH#482: a .env above the working directory belongs to something else.
    #[test]
    fn dot_env_is_read_from_the_working_directory_only() {
        let root = std::env::temp_dir().join(format!("shadoucmdb-dotenv-{}", uuid::Uuid::new_v4()));
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(root.join(".env"), "SHADOUCMDB_TEST_GH482_PARENT=loaded\n").unwrap();

        assert_eq!(load_dot_env_in(&sub).unwrap(), None);
        assert!(std::env::var("SHADOUCMDB_TEST_GH482_PARENT").is_err(), "parent .env must not be loaded");

        std::fs::write(sub.join(".env"), "SHADOUCMDB_TEST_GH482_CWD=loaded\n").unwrap();
        assert_eq!(load_dot_env_in(&sub).unwrap(), Some(sub.join(".env")));
        assert_eq!(std::env::var("SHADOUCMDB_TEST_GH482_CWD").as_deref(), Ok("loaded"));
        assert!(std::env::var("SHADOUCMDB_TEST_GH482_PARENT").is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
