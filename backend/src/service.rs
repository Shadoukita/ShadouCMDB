//! Running as a Windows Service.
//!
//! `shadoucmdb service install` registers the current executable with the
//! Service Control Manager. The SCM then starts `shadoucmdb ... service run`,
//! which hands control to the dispatcher; a Stop or Shutdown request triggers the
//! same graceful shutdown as Ctrl+C.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::Subcommand;

pub const DEFAULT_NAME: &str = "ShadouCMDB";

#[derive(Debug, Subcommand)]
pub enum ServiceCommand {
    /// Register shadoucmdb as a Windows Service (auto start, restart on failure). Run as Administrator.
    Install {
        /// Service name.
        #[arg(long, default_value = DEFAULT_NAME)]
        name: String,
        /// Account the service runs as. The default is the service's own virtual account,
        /// `NT SERVICE\<name>`: a SID no other service shares, without a password.
        #[arg(long)]
        account: Option<String>,
        /// Password for --account (not needed for LocalService, NetworkService or virtual accounts).
        #[arg(long)]
        password: Option<String>,
    },
    /// Stop and remove the Windows Service. Run as Administrator.
    Uninstall {
        #[arg(long, default_value = DEFAULT_NAME)]
        name: String,
    },
    /// Entry point used by the Service Control Manager; not for interactive use.
    Run {
        #[arg(long, default_value = DEFAULT_NAME)]
        name: String,
    },
}

/// Global options the service must be started with again (--env-file, --log-file).
pub struct LaunchOptions {
    pub env_file: Option<PathBuf>,
    pub log_file: Option<PathBuf>,
}

#[cfg(not(windows))]
pub fn run(_cmd: ServiceCommand, _launch: LaunchOptions) -> anyhow::Result<()> {
    anyhow::bail!(
        "`shadoucmdb service` manages a Windows Service and is only available on Windows. \
         On Linux, use the systemd unit in deploy/systemd/shadoucmdb.service."
    )
}

#[cfg(windows)]
pub use windows_impl::run;

#[cfg(windows)]
mod windows_impl {
    use std::ffi::OsString;
    use std::sync::{Arc, OnceLock};
    use std::time::Duration;

    use anyhow::Context;
    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept, ServiceErrorControl,
        ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
        ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_service::{define_windows_service, service_dispatcher};

    use super::{LaunchOptions, ServiceCommand, launch_arguments};

    static SERVICE_NAME: OnceLock<String> = OnceLock::new();

    pub fn run(cmd: ServiceCommand, launch: LaunchOptions) -> anyhow::Result<()> {
        match cmd {
            ServiceCommand::Install { name, account, password } => {
                let account = account.unwrap_or_else(|| super::virtual_account(&name));
                install(&name, &account, password, &launch)
            }
            ServiceCommand::Uninstall { name } => uninstall(&name),
            ServiceCommand::Run { name } => {
                let _ = SERVICE_NAME.set(name.clone());
                service_dispatcher::start(&name, ffi_service_main)
                    .context("could not connect to the Service Control Manager (this command is started by Windows, not by hand)")
            }
        }
    }

    fn install(name: &str, account: &str, password: Option<String>, launch: &LaunchOptions) -> anyhow::Result<()> {
        let manager = ServiceManager::local_computer(
            None::<&str>,
            ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
        )
        .context("could not open the Service Control Manager (run from an elevated prompt)")?;
        let executable_path = std::env::current_exe()?;
        let info = ServiceInfo {
            name: OsString::from(name),
            display_name: OsString::from("ShadouCMDB"),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: executable_path.clone(),
            launch_arguments: launch_arguments(name, launch)?,
            dependencies: vec![],
            account_name: Some(OsString::from(account)),
            account_password: password.map(OsString::from),
        };
        let service = manager
            .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
            .with_context(|| format!("could not create service {name}"))?;
        service.set_description("ShadouCMDB configuration management database: API and web UI")?;
        // Restart after 10 s on the first failures; reset the counter after a day.
        let restart = ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(10) };
        service.update_failure_actions(ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86_400)),
            reboot_msg: None,
            command: None,
            actions: Some(vec![restart.clone(), restart.clone(), restart]),
        })?;
        println!("Installed service {name} ({}), running as {account}", executable_path.display());
        println!("Start it with:  sc.exe start {name}   (or Start-Service {name})");
        Ok(())
    }

    fn uninstall(name: &str) -> anyhow::Result<()> {
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
            .context("could not open the Service Control Manager (run from an elevated prompt)")?;
        let service = manager
            .open_service(name, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE)
            .with_context(|| format!("service {name} not found"))?;
        if service.query_status()?.current_state != ServiceState::Stopped {
            let _ = service.stop();
            for _ in 0..30 {
                if service.query_status()?.current_state == ServiceState::Stopped {
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        service.delete()?;
        println!("Removed service {name}");
        Ok(())
    }

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_args: Vec<OsString>) {
        if let Err(err) = run_service() {
            tracing::error!(error = %format!("{err:#}"), "service failed");
        }
    }

    fn run_service() -> anyhow::Result<()> {
        let name = SERVICE_NAME.get().cloned().unwrap_or_else(|| super::DEFAULT_NAME.to_owned());
        let stop = Arc::new(tokio::sync::Notify::new());
        let stop_handler = stop.clone();
        let status_handle = service_control_handler::register(&name, move |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                stop_handler.notify_one();
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })?;
        let set = |state: ServiceState, accept: ServiceControlAccept, exit: u32, wait: Duration| {
            status_handle.set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: accept,
                exit_code: ServiceExitCode::Win32(exit),
                checkpoint: 0,
                wait_hint: wait,
                process_id: None,
            })
        };

        set(ServiceState::StartPending, ServiceControlAccept::empty(), 0, Duration::from_secs(10))?;
        let result = crate::config::Config::from_env().and_then(|cfg| {
            let runtime = tokio::runtime::Runtime::new()?;
            set(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN, 0, Duration::ZERO)?;
            tracing::info!(service = %name, "running as a Windows Service");
            runtime.block_on(crate::http::serve(cfg, async move {
                stop.notified().await;
                tracing::info!("stop requested by the Service Control Manager");
            }))
        });
        if let Err(err) = &result {
            tracing::error!(error = %format!("{err:#}"), "server exited with an error");
        }
        // ERROR_SERVICE_SPECIFIC_ERROR-style non-zero exit makes the SCM apply the restart policy.
        set(ServiceState::Stopped, ServiceControlAccept::empty(), if result.is_ok() { 0 } else { 1 }, Duration::ZERO)?;
        result
    }
}

/// The per-service virtual account Windows creates for service `name`.
#[cfg_attr(not(windows), allow(dead_code))]
fn virtual_account(name: &str) -> String {
    format!(r"NT SERVICE\{name}")
}

/// Arguments the SCM passes when it starts the service: the same global
/// options as the install command (with absolute paths), then `service run`.
#[cfg_attr(not(windows), allow(dead_code))]
fn launch_arguments(name: &str, launch: &LaunchOptions) -> anyhow::Result<Vec<OsString>> {
    let mut args = Vec::new();
    for (flag, path) in [("--env-file", &launch.env_file), ("--log-file", &launch.log_file)] {
        if let Some(p) = path {
            let abs = if p.is_absolute() { p.clone() } else { std::env::current_dir()?.join(p) };
            args.push(OsString::from(flag));
            args.push(abs.into_os_string());
        }
    }
    args.extend(["service", "run", "--name"].map(OsString::from));
    args.push(OsString::from(name));
    Ok(args)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(subcommand)]
        cmd: ServiceCommand,
    }

    #[test]
    fn install_defaults_to_the_services_own_virtual_account() {
        let Cli { cmd: ServiceCommand::Install { name, account, .. } } =
            Cli::parse_from(["x", "install", "--name", "CmdbTest"])
        else {
            panic!("not an install command");
        };
        assert_eq!(account, None);
        assert_eq!(virtual_account(&name), r"NT SERVICE\CmdbTest");
        assert_eq!(virtual_account(DEFAULT_NAME), r"NT SERVICE\ShadouCMDB");
    }

    #[test]
    fn an_explicit_account_is_kept() {
        let Cli { cmd: ServiceCommand::Install { account, .. } } =
            Cli::parse_from(["x", "install", "--account", r"CORP\svc-cmdb$"])
        else {
            panic!("not an install command");
        };
        assert_eq!(account.as_deref(), Some(r"CORP\svc-cmdb$"));
    }
}
