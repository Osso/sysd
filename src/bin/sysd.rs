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
use std::fs::OpenOptions;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

use peercred_ipc::{CallerInfo, Connection, Server};
use sysd::dbus::DbusServer;
use sysd::manager::Manager;
use sysd::pid1::{self, ShutdownType, SignalHandler, SysdSignal};
use sysd::protocol::{socket_path, Request, Response, UnitInfo};

/// Set up logging to both console and file
fn setup_logging(user_mode: bool) {
    let log_path = if user_mode {
        // User mode: /run/user/<uid>/sysd.log
        if let Some(uid) = std::env::var("XDG_RUNTIME_DIR").ok() {
            format!("{}/sysd.log", uid)
        } else {
            format!("/run/user/{}/sysd.log", nix::unistd::getuid())
        }
    } else {
        // System mode: /var/log/sysd.log
        "/var/log/sysd.log".to_string()
    };

    let mut dispatch = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{}][{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                message
            ))
        })
        .level(log::LevelFilter::Debug)
        .chain(std::io::stderr());

    // Try to add file output
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(&log_path) {
        dispatch = dispatch.chain(file);
        eprintln!("sysd: Logging to {}", log_path);
    } else {
        eprintln!(
            "sysd: Could not open log file {}, logging to stderr only",
            log_path
        );
    }

    if let Err(e) = dispatch.apply() {
        eprintln!("sysd: Failed to set up logging: {}", e);
        // Fall back to env_logger
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    }
}

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

    let is_pid1 = pid1::is_pid1();
    let user_mode = args.user;

    setup_logging(user_mode);
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

    // Take the socket activation receiver before wrapping manager in Arc
    let socket_activation_rx = manager.take_socket_activation_rx();

    // Take the timer fired receiver before wrapping manager in Arc
    let timer_rx = manager.take_timer_rx();

    // Take the path triggered receiver before wrapping manager in Arc
    let path_rx = manager.take_path_rx();

    // Take the oneshot completion receiver before wrapping manager in Arc
    let oneshot_completion_rx = manager.take_oneshot_completion_rx();

    let manager: SharedManager = Arc::new(RwLock::new(manager));

    // Shutdown flag to stop background tasks during shutdown
    let shutdown_flag = Arc::new(AtomicBool::new(false));

    // Spawn socket activation handler task
    if let Some(mut rx) = socket_activation_rx {
        let manager_sock = Arc::clone(&manager);
        let shutdown_sock = Arc::clone(&shutdown_flag);
        tokio::spawn(async move {
            while let Some(activation) = rx.recv().await {
                if shutdown_sock.load(Ordering::Relaxed) {
                    log::debug!("Socket activation handler stopping due to shutdown");
                    break;
                }
                let mut mgr = manager_sock.write().await;
                if let Err(e) = mgr.handle_socket_activation(activation).await {
                    log::error!("Socket activation failed: {}", e);
                }
            }
        });
    }

    // Spawn timer fired handler task
    if let Some(mut rx) = timer_rx {
        let manager_timer = Arc::clone(&manager);
        let shutdown_timer = Arc::clone(&shutdown_flag);
        tokio::spawn(async move {
            while let Some(fired) = rx.recv().await {
                if shutdown_timer.load(Ordering::Relaxed) {
                    log::debug!("Timer handler stopping due to shutdown");
                    break;
                }
                let mut mgr = manager_timer.write().await;
                if let Err(e) = mgr.handle_timer_fired(fired).await {
                    log::error!("Timer activation failed: {}", e);
                }
            }
        });
    }

    // Spawn path triggered handler task
    if let Some(mut rx) = path_rx {
        let manager_path = Arc::clone(&manager);
        let shutdown_path = Arc::clone(&shutdown_flag);
        tokio::spawn(async move {
            while let Some(triggered) = rx.recv().await {
                if shutdown_path.load(Ordering::Relaxed) {
                    log::debug!("Path handler stopping due to shutdown");
                    break;
                }
                let mut mgr = manager_path.write().await;
                if let Err(e) = mgr.handle_path_triggered(triggered).await {
                    log::error!("Path activation failed: {}", e);
                }
            }
        });
    }

    // Spawn oneshot completion handler task
    if let Some(mut rx) = oneshot_completion_rx {
        let manager_oneshot = Arc::clone(&manager);
        let shutdown_oneshot = Arc::clone(&shutdown_flag);
        tokio::spawn(async move {
            while let Some(completion) = rx.recv().await {
                if shutdown_oneshot.load(Ordering::Relaxed) {
                    log::debug!("Oneshot completion handler stopping due to shutdown");
                    break;
                }
                let mut mgr = manager_oneshot.write().await;
                mgr.handle_oneshot_completion(completion).await;
            }
        });
    }

    // D-Bus server initialization is deferred until after boot
    // because dbus-broker.service needs to start first
    // Spawn a task that retries D-Bus connection with backoff
    if !user_mode {
        let manager_dbus = Arc::clone(&manager);
        let shutdown_dbus = Arc::clone(&shutdown_flag);
        tokio::spawn(async move {
            // Wait a bit for boot to start dbus.socket and dbus-broker.service
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            let mut attempts = 0;
            let max_attempts = 30; // Try for ~60 seconds total
            let mut delay = std::time::Duration::from_millis(500);

            loop {
                // Stop retrying if shutdown is in progress
                if shutdown_dbus.load(Ordering::Relaxed) {
                    log::debug!("D-Bus retry loop stopping due to shutdown");
                    break;
                }
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
                            attempts,
                            e,
                            delay
                        );
                        tokio::time::sleep(delay).await;
                        // Exponential backoff, max 5 seconds
                        delay = std::cmp::min(delay * 2, std::time::Duration::from_secs(5));
                    }
                }
            }
        });
    } else {
        // User mode: connect to session bus
        let manager_dbus = Arc::clone(&manager);
        let shutdown_dbus = Arc::clone(&shutdown_flag);
        tokio::spawn(async move {
            // In user mode, session bus should already be available
            // Retry with backoff in case dbus-daemon is still starting
            let mut attempts = 0;
            let max_attempts = 20; // Try for ~30 seconds
            let mut delay = std::time::Duration::from_millis(200);

            loop {
                if shutdown_dbus.load(Ordering::Relaxed) {
                    log::debug!("D-Bus retry loop stopping due to shutdown");
                    break;
                }
                match DbusServer::new_session(Arc::clone(&manager_dbus)).await {
                    Ok(_server) => {
                        info!(
                            "D-Bus interface available on session bus at org.freedesktop.systemd1"
                        );
                        // Keep server alive
                        std::future::pending::<()>().await;
                    }
                    Err(e) => {
                        attempts += 1;
                        if attempts >= max_attempts {
                            log::warn!(
                                "Failed to start D-Bus server on session bus after {} attempts: {}",
                                attempts,
                                e
                            );
                            log::info!("User mode will continue without D-Bus interface");
                            break;
                        }
                        log::debug!(
                            "Session D-Bus not ready yet (attempt {}): {}, retrying in {:?}",
                            attempts,
                            e,
                            delay
                        );
                        tokio::time::sleep(delay).await;
                        delay = std::cmp::min(delay * 2, std::time::Duration::from_secs(3));
                    }
                }
            }
        });
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

    // Note: Zombie reaping is now handled by the Manager's reap() function
    // using waitpid(-1, WNOHANG). This avoids race conditions and preserves
    // actual exit codes. The Manager's background task (below) calls reap()
    // every 100ms which handles both service processes and orphaned processes.
    let _ = is_pid1; // Mark as used

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
        let shutdown_sig = Arc::clone(&shutdown_flag);
        tokio::spawn(async move {
            while let Some(sig) = rx.recv().await {
                match sig {
                    SysdSignal::Child => {
                        // Zombie reaping handled by dedicated task
                    }
                    SysdSignal::Term => {
                        info!("Received SIGTERM, initiating poweroff");
                        shutdown_sig.store(true, Ordering::Relaxed);
                        stop_all_services(&manager_sig).await;
                        pid1::shutdown(ShutdownType::Poweroff).await;
                    }
                    SysdSignal::Int => {
                        info!("Received SIGINT, initiating reboot");
                        shutdown_sig.store(true, Ordering::Relaxed);
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
    info!(
        "sysd{} listening on {}",
        if user_mode { " (user)" } else { "" },
        sock_path
    );

    // Boot to default target if requested
    if should_boot {
        let manager_boot = Arc::clone(&manager);
        tokio::spawn(async move {
            // Get default target and boot plan while holding lock briefly
            let (target, plan) = {
                let mgr = manager_boot.read().await;
                let target = match mgr.get_default_target() {
                    Ok(t) => t,
                    Err(e) => {
                        log::error!("No default target found: {}", e);
                        return;
                    }
                };
                // Release read lock, reacquire as write to get boot plan
                drop(mgr);
                let mut mgr = manager_boot.write().await;
                match mgr.get_boot_plan(&target).await {
                    Ok(p) => (target, p),
                    Err(e) => {
                        eprintln!("sysd: ERROR: Failed to get boot plan: {}", e);
                        log::error!("Failed to get boot plan: {}", e);
                        return;
                    }
                }
            };

            info!("Booting to target: {}", target);
            eprintln!("sysd: Boot plan: {} units", plan.len());
            info!("Boot plan: {} units", plan.len());
            let preview: Vec<_> = plan.iter().take(10).collect();
            eprintln!("sysd: First units: {:?}", preview);
            log::debug!("Boot plan order: {:?}", plan);

            // Start each unit, releasing the lock between starts to allow
            // socket activation and other background tasks to run
            for unit_name in &plan {
                eprintln!("sysd: Starting {}", unit_name);
                log::info!("Starting {}", unit_name);
                let mut mgr = manager_boot.write().await;
                match mgr.start(unit_name).await {
                    Ok(()) => log::info!("Started {}", unit_name),
                    Err(e) => {
                        eprintln!("sysd: FAILED to start {}: {}", unit_name, e);
                        log::warn!("Failed to start {}: {}", unit_name, e);
                    }
                }
                // Lock is released here when mgr goes out of scope
            }
            eprintln!("sysd: Boot complete");
            info!("Boot complete");
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
    if let Some(response) = special_request_response(&request, manager).await {
        return response;
    }

    match request {
        Request::List { user: _, unit_type } => list_response(manager, unit_type).await,
        Request::Start { name } => start_response(manager, &name).await,
        Request::StartAndWait { name } => start_and_wait_response(manager, &name).await,
        Request::Stop { name } => stop_response(manager, &name).await,
        Request::Restart { name } => restart_response(manager, &name).await,
        Request::Enable { name } => enable_response(manager, &name).await,
        Request::Disable { name } => disable_response(manager, &name).await,
        Request::IsEnabled { name } => is_enabled_response(manager, &name).await,
        Request::Status { name } => status_response(manager, &name).await,
        Request::Deps { name } => deps_response(manager, &name).await,
        Request::GetBootTarget => boot_target_response(manager).await,
        Request::Boot { dry_run } => boot_response(manager, dry_run).await,
        Request::ReloadUnitFiles => reload_units_response(manager).await,
        Request::SyncUnits => sync_units_response(manager).await,
        Request::SwitchTarget { target } => switch_target_response(manager, &target).await,
        Request::IsActive { name } => is_active_response(manager, &name).await,
        Request::Ping
        | Request::ImportEnvironment { .. }
        | Request::UnsetEnvironment { .. }
        | Request::ResetFailed => unreachable!(),
    }
}

async fn special_request_response(request: &Request, manager: &SharedManager) -> Option<Response> {
    match request {
        Request::Ping => Some(Response::Pong),
        Request::ImportEnvironment { vars } => {
            let mut mgr = manager.write().await;
            mgr.import_environment(vars.clone());
            Some(Response::Ok)
        }
        Request::UnsetEnvironment { names } => {
            let mut mgr = manager.write().await;
            mgr.unset_environment(names);
            Some(Response::Ok)
        }
        Request::ResetFailed => {
            let mut mgr = manager.write().await;
            mgr.reset_failed();
            Some(Response::Ok)
        }
        _ => None,
    }
}

async fn list_response(manager: &SharedManager, unit_type: Option<String>) -> Response {
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

async fn start_response(manager: &SharedManager, name: &str) -> Response {
    let mut mgr = manager.write().await;
    to_ok_response(mgr.start(name).await)
}

async fn stop_response(manager: &SharedManager, name: &str) -> Response {
    let mut mgr = manager.write().await;
    to_ok_response(mgr.stop(name).await)
}

async fn restart_response(manager: &SharedManager, name: &str) -> Response {
    let mut mgr = manager.write().await;
    to_ok_response(mgr.restart(name).await)
}

async fn start_and_wait_response(manager: &SharedManager, name: &str) -> Response {
    {
        let mut mgr = manager.write().await;
        if let Err(error) = mgr.start(name).await {
            return Response::Error(error.to_string());
        }
    }

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let mgr = manager.read().await;
        let Some(state) = mgr.status(name) else {
            return Response::Error(format!("Unit {} not found", name));
        };

        use sysd::manager::ActiveState;
        match state.active {
            ActiveState::Inactive => return Response::Ok,
            ActiveState::Failed => {
                let exit_code = state.exit_code.unwrap_or(0);
                return Response::Error(format!("Unit failed with exit code {}", exit_code));
            }
            _ => continue,
        }
    }
}

async fn enable_response(manager: &SharedManager, name: &str) -> Response {
    let mut mgr = manager.write().await;
    match mgr.enable(name).await {
        Ok(links) => {
            for link in &links {
                info!("Created symlink: {}", link.display());
            }
            Response::Ok
        }
        Err(error) => Response::Error(error.to_string()),
    }
}

async fn disable_response(manager: &SharedManager, name: &str) -> Response {
    let mut mgr = manager.write().await;
    match mgr.disable(name).await {
        Ok(links) => {
            for link in &links {
                info!("Removed symlink: {}", link.display());
            }
            Response::Ok
        }
        Err(error) => Response::Error(error.to_string()),
    }
}

async fn is_enabled_response(manager: &SharedManager, name: &str) -> Response {
    let mut mgr = manager.write().await;
    match mgr.is_enabled(name).await {
        Ok(enabled_state) => Response::EnabledState(enabled_state),
        Err(error) => Response::Error(error.to_string()),
    }
}

async fn status_response(manager: &SharedManager, name: &str) -> Response {
    let mgr = manager.read().await;
    match mgr.status(name) {
        Some(svc_state) => Response::Status(UnitInfo {
            name: name.to_string(),
            unit_type: "service".into(),
            state: format!("{:?}", svc_state.active),
            description: None,
        }),
        None => Response::Error(format!("unit not found: {}", name)),
    }
}

async fn deps_response(manager: &SharedManager, name: &str) -> Response {
    let mgr = manager.read().await;
    match mgr.get_unit(name) {
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

async fn boot_target_response(manager: &SharedManager) -> Response {
    match manager.read().await.get_default_target() {
        Ok(target) => Response::BootTarget(target),
        Err(error) => Response::Error(error.to_string()),
    }
}

async fn boot_response(manager: &SharedManager, dry_run: bool) -> Response {
    let mut mgr = manager.write().await;
    let target = match mgr.get_default_target() {
        Ok(target) => target,
        Err(error) => return Response::Error(error.to_string()),
    };
    if dry_run {
        return match mgr.get_boot_plan(&target).await {
            Ok(plan) => Response::BootPlan(plan),
            Err(error) => Response::Error(error.to_string()),
        };
    }

    match mgr.start_with_deps(&target).await {
        Ok(started) => Response::BootPlan(started),
        Err(error) => Response::Error(error.to_string()),
    }
}

async fn reload_units_response(manager: &SharedManager) -> Response {
    let mut mgr = manager.write().await;
    match mgr.reload_units().await {
        Ok(count) => {
            info!("Reloaded {} unit files", count);
            Response::Ok
        }
        Err(error) => Response::Error(error.to_string()),
    }
}

async fn sync_units_response(manager: &SharedManager) -> Response {
    let mut mgr = manager.write().await;
    match mgr.sync_units().await {
        Ok(restarted) => {
            if restarted.is_empty() {
                info!("All units in sync");
            } else {
                info!(
                    "Restarted {} changed units: {:?}",
                    restarted.len(),
                    restarted
                );
            }
            Response::BootPlan(restarted)
        }
        Err(error) => Response::Error(error.to_string()),
    }
}

async fn switch_target_response(manager: &SharedManager, target: &str) -> Response {
    let mut mgr = manager.write().await;
    match mgr.switch_target(target).await {
        Ok(stopped) => {
            info!("Switched to {}, stopped {} units", target, stopped.len());
            Response::BootPlan(stopped)
        }
        Err(error) => Response::Error(error.to_string()),
    }
}

async fn is_active_response(manager: &SharedManager, name: &str) -> Response {
    let mgr = manager.read().await;
    match mgr.status(name) {
        Some(state) => Response::ActiveState(format!("{:?}", state.active).to_lowercase()),
        None => Response::ActiveState("unknown".to_string()),
    }
}

fn to_ok_response<T, E: ToString>(result: Result<T, E>) -> Response {
    match result {
        Ok(_) => Response::Ok,
        Err(error) => Response::Error(error.to_string()),
    }
}
