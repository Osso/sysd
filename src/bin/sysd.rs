//! sysd - Minimal systemd-compatible init daemon
//!
//! Listens on /run/sysd.sock for commands from sysdctl.
//! Provides D-Bus interface at org.freedesktop.systemd1 for logind compatibility.
//!
//! When running as PID 1:
//! - Mounts essential filesystems
//! - Reaps zombie processes
//! - Handles signals for shutdown
//!
//! User mode (--user):
//! - Runs per-user service manager
//! - Uses ~/.config/systemd/user and /usr/lib/systemd/user
//! - Socket at /run/user/<uid>/sysd.sock

use clap::Parser;
use log::info;
use std::sync::Arc;
use tokio::sync::RwLock;

use peercred_ipc::{CallerInfo, Connection, Server};
use sysd::dbus::DbusServer;
use sysd::manager::Manager;
use sysd::pid1::{self, ShutdownType, SignalHandler, SysdSignal, ZombieReaper};
use sysd::protocol::{socket_path, Request, Response, UnitInfo};

#[derive(Parser)]
#[command(name = "sysd")]
#[command(about = "Minimal systemd-compatible init daemon")]
#[command(
    long_about = "sysd is a minimal init system that parses systemd unit files \
    and manages services. It listens on /run/sysd.sock for commands from sysdctl."
)]
struct Args {
    /// Run in foreground (don't daemonize)
    #[arg(long, short = 'f')]
    foreground: bool,

    /// Run as user service manager (like systemd --user)
    #[arg(long)]
    user: bool,

    /// Don't boot to default target (only when running as PID 1)
    #[arg(long)]
    no_boot: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Start daemon and boot to default target
    Boot,
}

/// Shared manager state accessible from IPC and D-Bus
type SharedManager = Arc<RwLock<Manager>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let is_pid1 = pid1::is_pid1();
    let user_mode = args.user;
    // Boot when: explicit `boot` subcommand, OR running as PID 1 (unless --no-boot)
    let should_boot = matches!(args.command, Some(Command::Boot)) || (is_pid1 && !args.no_boot);

    // User mode validation
    if user_mode && is_pid1 {
        log::error!("Cannot run in --user mode as PID 1");
        std::process::exit(1);
    }

    // PID 1: Initialize essential filesystems and environment
    if is_pid1 {
        if let Err(e) = pid1::init() {
            log::error!("PID 1 initialization failed: {}", e);
            // Continue anyway - some mounts might have succeeded
        }
    }

    // User mode: Ensure runtime directory exists
    if user_mode {
        if let Err(e) = Manager::ensure_runtime_dir() {
            log::warn!("Failed to ensure runtime directory: {}", e);
        }
    }

    // Create manager in appropriate mode
    let mut manager = if user_mode {
        info!("Starting user service manager");
        Manager::new_user()
    } else {
        Manager::new()
    };

    // Initialize notify socket for Type=notify services
    if let Err(e) = manager.init_notify_socket() {
        log::warn!(
            "Failed to create notify socket: {} (Type=notify services won't work)",
            e
        );
    }

    // Load mount units from /etc/fstab (replaces fstab-generator)
    // Only in system mode (not user mode)
    if !user_mode {
        match manager.load_fstab() {
            Ok(count) if count > 0 => {
                info!("Loaded {} mount units from /etc/fstab", count);
            }
            Ok(_) => {
                log::debug!("No mount units loaded from fstab");
            }
            Err(e) => {
                log::warn!("Failed to load fstab: {}", e);
            }
        }

        // Load getty units from /proc/cmdline (replaces getty-generator)
        match manager.load_gettys() {
            Ok(count) if count > 0 => {
                info!("Loaded {} getty units from kernel cmdline", count);
            }
            Ok(_) => {
                log::debug!("No getty units loaded");
            }
            Err(e) => {
                log::warn!("Failed to load gettys: {}", e);
            }
        }
    }

    let manager: SharedManager = Arc::new(RwLock::new(manager));

    // D-Bus server initialization is deferred until after boot
    // because dbus-broker.service needs to start first
    // Spawn a task that retries D-Bus connection with backoff
    if !user_mode {
        let manager_dbus = Arc::clone(&manager);
        tokio::spawn(async move {
            // Wait a bit for boot to start dbus.socket and dbus-broker.service
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            let mut attempts = 0;
            let max_attempts = 30; // Try for ~60 seconds total
            let mut delay = std::time::Duration::from_millis(500);

            loop {
                match DbusServer::new(Arc::clone(&manager_dbus)).await {
                    Ok(_server) => {
                        info!("D-Bus interface available at org.freedesktop.systemd1");
                        // Keep server alive - it's moved into this task
                        // The connection persists as long as this task runs
                        std::future::pending::<()>().await;
                    }
                    Err(e) => {
                        attempts += 1;
                        if attempts >= max_attempts {
                            log::warn!(
                                "Failed to start D-Bus server after {} attempts: {} (logind integration unavailable)",
                                attempts, e
                            );
                            break;
                        }
                        log::debug!(
                            "D-Bus not ready yet (attempt {}): {}, retrying in {:?}",
                            attempts, e, delay
                        );
                        tokio::time::sleep(delay).await;
                        // Exponential backoff, max 5 seconds
                        delay = std::cmp::min(delay * 2, std::time::Duration::from_secs(5));
                    }
                }
            }
        });
    } else {
        // TODO: User mode D-Bus on session bus
        log::debug!("D-Bus not enabled in user mode (not yet implemented)");
    }

    // PID 1: Set up signal handler for shutdown/reboot
    let signal_rx = if is_pid1 {
        match SignalHandler::new() {
            Ok(handler) => Some(handler.spawn_forwarder()),
            Err(e) => {
                log::error!("Failed to set up signal handlers: {}", e);
                None
            }
        }
    } else {
        None
    };

    // PID 1: Spawn zombie reaper task
    if is_pid1 {
        tokio::spawn(async move {
            let reaper = ZombieReaper::new();
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
            loop {
                interval.tick().await;
                reaper.reap_all();
            }
        });
    }

    // Spawn background task for processing notify messages, D-Bus readiness, watchdog, and service reaping
    let manager_bg = Arc::clone(&manager);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            interval.tick().await;
            let mut mgr = manager_bg.write().await;
            mgr.process_notify().await;
            mgr.process_dbus_ready().await;
            mgr.process_watchdog().await;
            mgr.reap().await;
            mgr.process_restarts().await;
        }
    });

    // PID 1: Spawn signal handler task
    if let Some(mut rx) = signal_rx {
        let manager_sig = Arc::clone(&manager);
        tokio::spawn(async move {
            while let Some(sig) = rx.recv().await {
                match sig {
                    SysdSignal::Child => {
                        // Zombie reaping handled by dedicated task
                    }
                    SysdSignal::Term => {
                        info!("Received SIGTERM, initiating poweroff");
                        stop_all_services(&manager_sig).await;
                        pid1::shutdown(ShutdownType::Poweroff).await;
                    }
                    SysdSignal::Int => {
                        info!("Received SIGINT, initiating reboot");
                        stop_all_services(&manager_sig).await;
                        pid1::shutdown(ShutdownType::Reboot).await;
                    }
                    SysdSignal::Hup => {
                        info!("Received SIGHUP, reloading unit files");
                        let mut mgr = manager_sig.write().await;
                        match mgr.reload_units().await {
                            Ok(count) => info!("Reloaded {} unit files", count),
                            Err(e) => log::error!("Failed to reload units: {}", e),
                        }
                    }
                    SysdSignal::Usr1 => {
                        info!("Received SIGUSR1, dumping state");
                        let mgr = manager_sig.read().await;
                        for (name, state) in mgr.list() {
                            info!("  {}: {:?}", name, state.active);
                        }
                    }
                }
            }
        });
    }

    // Determine socket path based on mode
    let sock_path = socket_path(user_mode);
    let server = Server::bind(&sock_path)?;
    info!("sysd{} listening on {}", if user_mode { " (user)" } else { "" }, sock_path);

    // Boot to default target if requested
    if should_boot {
        let manager_boot = Arc::clone(&manager);
        tokio::spawn(async move {
            let mut mgr = manager_boot.write().await;
            match mgr.get_default_target() {
                Ok(target) => {
                    info!("Booting to target: {}", target);
                    match mgr.get_boot_plan(&target).await {
                        Ok(plan) => {
                            info!("Boot plan: {} units", plan.len());
                            for unit_name in plan {
                                if let Err(e) = mgr.start(&unit_name).await {
                                    log::warn!("Failed to start {}: {}", unit_name, e);
                                }
                            }
                            info!("Boot complete");
                        }
                        Err(e) => log::error!("Failed to get boot plan: {}", e),
                    }
                }
                Err(e) => log::error!("No default target found: {}", e),
            }
        });
    }

    loop {
        match server.accept().await {
            Ok((conn, caller)) => {
                let manager = Arc::clone(&manager);
                tokio::spawn(handle_connection(conn, caller, manager));
            }
            Err(e) => {
                log::error!("accept error: {}", e);
            }
        }
    }
}

/// Stop all running services before shutdown
async fn stop_all_services(manager: &SharedManager) {
    let mgr = manager.read().await;
    let running: Vec<String> = mgr
        .list()
        .filter(|(_, state)| state.is_active())
        .map(|(name, _)| name.clone())
        .collect();
    drop(mgr);

    for name in running {
        info!("Stopping {} for shutdown", name);
        let mut mgr = manager.write().await;
        if let Err(e) = mgr.stop(&name).await {
            log::warn!("Failed to stop {}: {}", name, e);
        }
    }
}

async fn handle_connection(mut conn: Connection, caller: CallerInfo, manager: SharedManager) {
    info!(
        "connection from uid={} pid={} exe={:?}",
        caller.uid, caller.pid, caller.exe
    );

    let request: Request = match conn.read().await {
        Ok(r) => r,
        Err(e) => {
            log::error!("read error: {}", e);
            let _ = conn.write(&Response::Error("invalid request".into())).await;
            return;
        }
    };

    let response = handle_request(request, &manager).await;
    if let Err(e) = conn.write(&response).await {
        log::error!("write error: {}", e);
    }
}

async fn handle_request(request: Request, manager: &SharedManager) -> Response {
    match request {
        Request::Ping => Response::Pong,

        Request::List { user: _, unit_type } => {
            let mgr = manager.read().await;
            let units: Vec<UnitInfo> = mgr
                .list_units()
                .into_iter()
                .filter(|(_, unit, _)| {
                    unit_type
                        .as_ref()
                        .map_or(true, |t| unit.unit_type() == t.as_str())
                })
                .map(|(name, unit, state)| UnitInfo {
                    name: name.clone(),
                    unit_type: unit.unit_type().into(),
                    state: state
                        .map(|s| format!("{:?}", s.active))
                        .unwrap_or_else(|| "inactive".into()),
                    description: unit.unit_section().description.clone(),
                })
                .collect();
            Response::Units(units)
        }

        Request::Start { name } => {
            let mut mgr = manager.write().await;
            match mgr.start(&name).await {
                Ok(_) => Response::Ok,
                Err(e) => Response::Error(e.to_string()),
            }
        }

        Request::Stop { name } => {
            let mut mgr = manager.write().await;
            match mgr.stop(&name).await {
                Ok(_) => Response::Ok,
                Err(e) => Response::Error(e.to_string()),
            }
        }

        Request::Restart { name } => {
            let mut mgr = manager.write().await;
            match mgr.restart(&name).await {
                Ok(_) => Response::Ok,
                Err(e) => Response::Error(e.to_string()),
            }
        }

        Request::Enable { name } => {
            let mut mgr = manager.write().await;
            match mgr.enable(&name).await {
                Ok(links) => {
                    for link in &links {
                        info!("Created symlink: {}", link.display());
                    }
                    Response::Ok
                }
                Err(e) => Response::Error(e.to_string()),
            }
        }

        Request::Disable { name } => {
            let mut mgr = manager.write().await;
            match mgr.disable(&name).await {
                Ok(links) => {
                    for link in &links {
                        info!("Removed symlink: {}", link.display());
                    }
                    Response::Ok
                }
                Err(e) => Response::Error(e.to_string()),
            }
        }

        Request::IsEnabled { name } => {
            let mut mgr = manager.write().await;
            match mgr.is_enabled(&name).await {
                Ok(enabled_state) => Response::EnabledState(enabled_state),
                Err(e) => Response::Error(e.to_string()),
            }
        }

        Request::Status { name } => {
            let mgr = manager.read().await;
            match mgr.status(&name) {
                Some(svc_state) => Response::Status(UnitInfo {
                    name: name.clone(),
                    unit_type: "service".into(),
                    state: format!("{:?}", svc_state.active),
                    description: None,
                }),
                None => Response::Error(format!("unit not found: {}", name)),
            }
        }

        Request::Deps { name } => {
            let mgr = manager.read().await;
            match mgr.get_unit(&name) {
                Some(unit) => {
                    let mut deps = Vec::new();
                    let section = unit.unit_section();
                    deps.extend(section.requires.iter().cloned());
                    deps.extend(section.wants.iter().cloned());
                    deps.extend(section.after.iter().cloned());
                    Response::Deps(deps)
                }
                None => Response::Error(format!("unit not found: {}", name)),
            }
        }

        Request::GetBootTarget => match manager.read().await.get_default_target() {
            Ok(target) => Response::BootTarget(target),
            Err(e) => Response::Error(e.to_string()),
        },

        Request::Boot { dry_run } => {
            if dry_run {
                let mut mgr = manager.write().await;
                match mgr.get_default_target() {
                    Ok(target) => match mgr.get_boot_plan(&target).await {
                        Ok(plan) => Response::BootPlan(plan),
                        Err(e) => Response::Error(e.to_string()),
                    },
                    Err(e) => Response::Error(e.to_string()),
                }
            } else {
                let mut mgr = manager.write().await;
                match mgr.get_default_target() {
                    Ok(target) => match mgr.start_with_deps(&target).await {
                        Ok(started) => Response::BootPlan(started),
                        Err(e) => Response::Error(e.to_string()),
                    },
                    Err(e) => Response::Error(e.to_string()),
                }
            }
        }

        Request::ReloadUnitFiles => {
            let mut mgr = manager.write().await;
            match mgr.reload_units().await {
                Ok(count) => {
                    info!("Reloaded {} unit files", count);
                    Response::Ok
                }
                Err(e) => Response::Error(e.to_string()),
            }
        }

        Request::SyncUnits => {
            let mut mgr = manager.write().await;
            match mgr.sync_units().await {
                Ok(restarted) => {
                    if restarted.is_empty() {
                        info!("All units in sync");
                    } else {
                        info!("Restarted {} changed units: {:?}", restarted.len(), restarted);
                    }
                    Response::BootPlan(restarted) // Reuse BootPlan for list of unit names
                }
                Err(e) => Response::Error(e.to_string()),
            }
        }

        Request::SwitchTarget { target } => {
            let mut mgr = manager.write().await;
            match mgr.switch_target(&target).await {
                Ok(stopped) => {
                    info!("Switched to {}, stopped {} units", target, stopped.len());
                    Response::BootPlan(stopped) // List of stopped units
                }
                Err(e) => Response::Error(e.to_string()),
            }
        }
    }
}
