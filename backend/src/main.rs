//! `shadoucmdb`: the ShadouCMDB server and its admin commands in one binary.

mod api;
mod auth;
mod config;
mod data;
mod db;
mod http;
mod logging;
mod modules;
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
    /// Load reference data (idempotent).
    Seed {
        /// Also load a small demo inventory into a database without CIs.
        #[arg(long)]
        demo: bool,
    },
    /// Run schema acceptance checks inside a rolled-back transaction.
    Verify,
    /// Create a user with the built-in Administrator profile (first install or lost access).
    CreateAdmin(auth::cli::CreateAdminArgs),
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
            Command::CreateAdmin(_) => "create-admin",
            Command::Openapi { .. } => "openapi",
            Command::Service(_) => "service",
        }
    }
}

fn load_env_file(path: Option<&PathBuf>) -> anyhow::Result<()> {
    match path {
        Some(p) => {
            dotenvy::from_path(p).with_context(|| format!("cannot read env file {}", p.display()))?;
        }
        None => match dotenvy::dotenv() {
            Ok(_) => {}
            Err(e) if e.not_found() => {}
            Err(e) => return Err(e).context("cannot read .env"),
        },
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
            runtime()?.block_on(db::migrate(&cfg.database, adopt_drizzle))
        }
        Command::Seed { demo } => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(seed::run(&cfg.database, demo))
        }
        Command::Verify => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(verify::run(&cfg.database))
        }
        Command::CreateAdmin(args) => {
            let cfg = Config::from_env()?;
            runtime()?.block_on(auth::cli::create_admin(&cfg.database, args))
        }
        Command::Service(cmd) => service::run(cmd, launch),
        Command::Openapi { .. } => unreachable!("handled above"),
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
