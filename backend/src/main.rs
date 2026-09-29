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
    /// environment win). Without it, ./.env is loaded if it exists.
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
        /// Report missing rows (gaps in chainSeq) without failing, e.g. after retention pruning.
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
            Command::Openapi { .. } => "openapi",
            Command::Service(_) => "service",
        }
    }
}

fn load_env_file(path: Option<&PathBuf>) -> anyhow::Result<()> {
    let loaded = match path {
        Some(p) => {
            dotenvy::from_path(p).with_context(|| format!("cannot read env file {}", p.display()))?;
            Some(p.clone())
        }
        None => match dotenvy::dotenv() {
            Ok(p) => Some(p),
            Err(e) if e.not_found() => None,
            Err(e) => return Err(e).context("cannot read .env"),
        },
    };
    if let Some(p) = loaded {
        config::set_env_file(p);
    }
    Ok(())
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().enable_all().build()?)
}

fn run(cli: Cli) -> anyhow::Result<()> {
    load_env_file(cli.env_file.as_ref())?;
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
            runtime()?.block_on(maintenance::backup::run(&cfg.database, args))
        }
        // These rebuild or drop the schema: like `migrate`, they connect as the
        // schema owner when MIGRATION_DATABASE_URL is set.
        Command::Restore(args) => {
            let cfg = Config::from_env()?;
            let encryption = cfg.encryption.clone();
            let db = cfg.schema_owner_database()?;
            runtime()?.block_on(maintenance::restore::run(&db, &encryption, args))
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
