//! `shadoucmdb`: the ShadouCMDB server and its admin commands in one binary.

mod config;
mod db;
mod http;
mod logging;
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
        Command::Service(cmd) => service::run(cmd, launch),
    }
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
