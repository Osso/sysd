// sysd - Minimal systemd-compatible init daemon
//
// Listens on /run/sysd.sock for commands from sysdctl.
// Provides D-Bus interface at org.freedesktop.systemd1 for logind compatibility.
//
// When running as PID 1:
// - Mounts essential filesystems
// - Reaps zombie processes
// - Handles signals for shutdown
//
// User mode (--user):
// - Runs per-user service manager
// - Uses ~/.config/systemd/user and /usr/lib/systemd/user
// - Socket at /run/user/<uid>/sysd.sock

use clap::Parser;
use log::info;
use std::fs::OpenOptions;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

use peercred_ipc::Server;
use sysd::dbus::DbusServer;
use sysd::manager::Manager;
use sysd::pid1::{self, ShutdownType, SignalHandler, SysdSignal};
use sysd::protocol::socket_path;

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
    let (is_pid1, user_mode, should_boot) = runtime_modes(&args);
    initialize_environment(is_pid1, user_mode);
    let mut manager = create_manager(user_mode);
    let socket_activation_rx = manager.take_socket_activation_rx();
    let timer_rx = manager.take_timer_rx();
    let path_rx = manager.take_path_rx();
    let oneshot_completion_rx = manager.take_oneshot_completion_rx();
    let manager: SharedManager = Arc::new(RwLock::new(manager));
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    spawn_event_handlers(
        Arc::clone(&manager),
        Arc::clone(&shutdown_flag),
        socket_activation_rx,
        timer_rx,
        oneshot_completion_rx,
    );
    if let Some(rx) = path_rx {
        spawn_manager_result_handler(
            rx,
            Arc::clone(&manager),
            Arc::clone(&shutdown_flag),
            "Path handler stopping due to shutdown",
            "Path activation failed",
            |mgr, triggered| Box::pin(mgr.handle_path_triggered(triggered)),
        );
    }
    spawn_dbus_retry_task(user_mode, Arc::clone(&manager), Arc::clone(&shutdown_flag));
    spawn_background_maintenance(Arc::clone(&manager));
    spawn_signal_handler(is_pid1, Arc::clone(&manager), Arc::clone(&shutdown_flag));
    maybe_spawn_boot_task(should_boot, Arc::clone(&manager));
    serve_requests(user_mode, manager).await
}

fn runtime_modes(args: &Args) -> (bool, bool, bool) {
    let is_pid1 = pid1::is_pid1();
    let user_mode = args.user;
    let should_boot = matches!(args.command, Some(Command::Boot)) || (is_pid1 && !args.no_boot);
    (is_pid1, user_mode, should_boot)
}

fn initialize_environment(is_pid1: bool, user_mode: bool) {
    setup_logging(user_mode);
    validate_mode(is_pid1, user_mode);
    if is_pid1 {
        initialize_pid1();
    }
    if user_mode {
        ensure_user_runtime_dir();
    }
}

fn validate_mode(is_pid1: bool, user_mode: bool) {
    if !(user_mode && is_pid1) {
        return;
    }
    log::error!("Cannot run in --user mode as PID 1");
    std::process::exit(1);
}

fn initialize_pid1() {
    if let Err(e) = pid1::init() {
        log::error!("PID 1 initialization failed: {}", e);
    }
}

fn ensure_user_runtime_dir() {
    if let Err(e) = Manager::ensure_runtime_dir() {
        log::warn!("Failed to ensure runtime directory: {}", e);
    }
}

fn create_manager(user_mode: bool) -> Manager {
    let mut manager = if user_mode {
        info!("Starting user service manager");
        Manager::new_user()
    } else {
        Manager::new()
    };
    initialize_notify_socket(&mut manager);
    if !user_mode {
        load_legacy_mount_and_getty_units(&mut manager);
    }
    manager
}

fn initialize_notify_socket(manager: &mut Manager) {
    if let Err(e) = manager.init_notify_socket() {
        log::warn!(
            "Failed to create notify socket: {} (Type=notify services won't work)",
            e
        );
    }
}

fn load_legacy_mount_and_getty_units(manager: &mut Manager) {
    log_fstab_load_result(manager.load_fstab());
    log_getty_load_result(manager.load_gettys());
}

fn log_fstab_load_result(result: Result<usize, sysd::manager::ManagerError>) {
    match result {
        Ok(count) if count > 0 => info!("Loaded {} mount units from /etc/fstab", count),
        Ok(_) => log::debug!("No mount units loaded from fstab"),
        Err(e) => log::warn!("Failed to load fstab: {}", e),
    }
}

fn log_getty_load_result(result: Result<usize, sysd::manager::ManagerError>) {
    match result {
        Ok(count) if count > 0 => info!("Loaded {} getty units from kernel cmdline", count),
        Ok(_) => log::debug!("No getty units loaded"),
        Err(e) => log::warn!("Failed to load gettys: {}", e),
    }
}

type ManagerResultFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), sysd::manager::ManagerError>> + Send + 'a>>;

fn spawn_event_handlers(
    manager: SharedManager,
    shutdown_flag: Arc<AtomicBool>,
    socket_activation_rx: Option<mpsc::Receiver<sysd::manager::SocketActivation>>,
    timer_rx: Option<mpsc::Receiver<sysd::manager::TimerFired>>,
    oneshot_completion_rx: Option<mpsc::Receiver<sysd::manager::OneshotCompletion>>,
) {
    spawn_socket_handler(
        socket_activation_rx,
        Arc::clone(&manager),
        Arc::clone(&shutdown_flag),
    );
    spawn_timer_handler(timer_rx, Arc::clone(&manager), Arc::clone(&shutdown_flag));
    spawn_oneshot_handler(oneshot_completion_rx, manager, shutdown_flag);
}

fn spawn_socket_handler(
    socket_activation_rx: Option<mpsc::Receiver<sysd::manager::SocketActivation>>,
    manager: SharedManager,
    shutdown_flag: Arc<AtomicBool>,
) {
    let Some(rx) = socket_activation_rx else {
        return;
    };
    spawn_manager_result_handler(
        rx,
        manager,
        shutdown_flag,
        "Socket activation handler stopping due to shutdown",
        "Socket activation failed",
        |mgr, activation| Box::pin(mgr.handle_socket_activation(activation)),
    );
}

fn spawn_timer_handler(
    timer_rx: Option<mpsc::Receiver<sysd::manager::TimerFired>>,
    manager: SharedManager,
    shutdown_flag: Arc<AtomicBool>,
) {
    let Some(rx) = timer_rx else {
        return;
    };
    spawn_manager_result_handler(
        rx,
        manager,
        shutdown_flag,
        "Timer handler stopping due to shutdown",
        "Timer activation failed",
        |mgr, fired| Box::pin(mgr.handle_timer_fired(fired)),
    );
}

fn spawn_oneshot_handler(
    oneshot_completion_rx: Option<mpsc::Receiver<sysd::manager::OneshotCompletion>>,
    manager: SharedManager,
    shutdown_flag: Arc<AtomicBool>,
) {
    let Some(rx) = oneshot_completion_rx else {
        return;
    };
    spawn_manager_result_handler(
        rx,
        manager,
        shutdown_flag,
        "Oneshot completion handler stopping due to shutdown",
        "Oneshot completion handling failed",
        |mgr, completion| {
            Box::pin(async move {
                mgr.handle_oneshot_completion(completion).await;
                Ok(())
            })
        },
    );
}

fn spawn_manager_result_handler<T, F>(
    mut rx: mpsc::Receiver<T>,
    manager: SharedManager,
    shutdown_flag: Arc<AtomicBool>,
    stop_message: &'static str,
    error_message: &'static str,
    mut handler: F,
) where
    T: Send + 'static,
    F: for<'a> FnMut(&'a mut Manager, T) -> ManagerResultFuture<'a> + Send + 'static,
{
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if shutdown_flag.load(Ordering::Relaxed) {
                log::debug!("{}", stop_message);
                break;
            }
            let mut mgr = manager.write().await;
            if let Err(e) = handler(&mut mgr, msg).await {
                log::error!("{}: {}", error_message, e);
            }
        }
    });
}

fn spawn_dbus_retry_task(user_mode: bool, manager: SharedManager, shutdown_flag: Arc<AtomicBool>) {
    if user_mode {
        tokio::spawn(run_session_dbus_retry_loop(manager, shutdown_flag));
    } else {
        tokio::spawn(run_system_dbus_retry_loop(manager, shutdown_flag));
    }
}

async fn run_system_dbus_retry_loop(manager: SharedManager, shutdown_flag: Arc<AtomicBool>) {
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let mut attempts = 0;
    let max_attempts = 30;
    let mut delay = std::time::Duration::from_millis(500);
    while !shutdown_flag.load(Ordering::Relaxed) {
        let Err(e) = DbusServer::new(Arc::clone(&manager)).await else {
            info!("D-Bus interface available at org.freedesktop.systemd1");
            std::future::pending::<()>().await;
            return;
        };
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
        delay = std::cmp::min(delay * 2, std::time::Duration::from_secs(5));
    }
}

async fn run_session_dbus_retry_loop(manager: SharedManager, shutdown_flag: Arc<AtomicBool>) {
    let mut attempts = 0;
    let max_attempts = 20;
    let mut delay = std::time::Duration::from_millis(200);
    while !shutdown_flag.load(Ordering::Relaxed) {
        let Err(e) = DbusServer::new_session(Arc::clone(&manager)).await else {
            info!("D-Bus interface available on session bus at org.freedesktop.systemd1");
            std::future::pending::<()>().await;
            return;
        };
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

fn spawn_signal_handler(is_pid1: bool, manager: SharedManager, shutdown_flag: Arc<AtomicBool>) {
    let Some(mut signal_rx) = signal_receiver(is_pid1) else {
        return;
    };
    tokio::spawn(async move {
        while let Some(sig) = signal_rx.recv().await {
            handle_signal(sig, &manager, &shutdown_flag).await;
        }
    });
}

fn signal_receiver(is_pid1: bool) -> Option<mpsc::Receiver<SysdSignal>> {
    if !is_pid1 {
        return None;
    }
    match SignalHandler::new() {
        Ok(handler) => Some(handler.spawn_forwarder()),
        Err(e) => {
            log::error!("Failed to set up signal handlers: {}", e);
            None
        }
    }
}

async fn handle_signal(sig: SysdSignal, manager: &SharedManager, shutdown_flag: &Arc<AtomicBool>) {
    match sig {
        SysdSignal::Child => {}
        SysdSignal::Term => {
            shutdown_from_signal(manager, shutdown_flag, ShutdownType::Poweroff).await
        }
        SysdSignal::Int => shutdown_from_signal(manager, shutdown_flag, ShutdownType::Reboot).await,
        SysdSignal::Hup => reload_units_from_signal(manager).await,
        SysdSignal::Usr1 => dump_state_from_signal(manager).await,
    }
}

async fn shutdown_from_signal(
    manager: &SharedManager,
    shutdown_flag: &Arc<AtomicBool>,
    shutdown_type: ShutdownType,
) {
    match shutdown_type {
        ShutdownType::Poweroff => info!("Received SIGTERM, initiating poweroff"),
        ShutdownType::Reboot => info!("Received SIGINT, initiating reboot"),
        ShutdownType::Halt => info!("Received signal requesting halt"),
    }
    shutdown_flag.store(true, Ordering::Relaxed);
    stop_all_services(manager).await;
    pid1::shutdown(shutdown_type).await;
}

async fn reload_units_from_signal(manager: &SharedManager) {
    info!("Received SIGHUP, reloading unit files");
    let mut mgr = manager.write().await;
    match mgr.reload_units().await {
        Ok(count) => info!("Reloaded {} unit files", count),
        Err(e) => log::error!("Failed to reload units: {}", e),
    }
}

async fn dump_state_from_signal(manager: &SharedManager) {
    info!("Received SIGUSR1, dumping state");
    let mgr = manager.read().await;
    for (name, state) in mgr.list() {
        info!("  {}: {:?}", name, state.active);
    }
}

fn spawn_background_maintenance(manager: SharedManager) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            interval.tick().await;
            let mut mgr = manager.write().await;
            mgr.process_notify().await;
            mgr.process_dbus_ready().await;
            mgr.process_watchdog().await;
            mgr.reap().await;
            mgr.process_restarts().await;
        }
    });
}

fn maybe_spawn_boot_task(should_boot: bool, manager: SharedManager) {
    if !should_boot {
        return;
    }
    tokio::spawn(async move {
        boot_to_default_target(&manager).await;
    });
}

async fn boot_to_default_target(manager: &SharedManager) {
    let Some((target, plan)) = resolve_boot_target_and_plan(manager).await else {
        return;
    };
    info!("Booting to target: {}", target);
    eprintln!("sysd: Boot plan: {} units", plan.len());
    info!("Boot plan: {} units", plan.len());
    let preview: Vec<_> = plan.iter().take(10).collect();
    eprintln!("sysd: First units: {:?}", preview);
    log::debug!("Boot plan order: {:?}", plan);
    start_boot_plan_units(manager, &plan).await;
    eprintln!("sysd: Boot complete");
    info!("Boot complete");
}

async fn resolve_boot_target_and_plan(manager: &SharedManager) -> Option<(String, Vec<String>)> {
    let target = {
        let mgr = manager.read().await;
        match mgr.get_default_target() {
            Ok(target) => target,
            Err(e) => {
                log::error!("No default target found: {}", e);
                return None;
            }
        }
    };
    let mut mgr = manager.write().await;
    match mgr.get_boot_plan(&target).await {
        Ok(plan) => Some((target, plan)),
        Err(e) => {
            eprintln!("sysd: ERROR: Failed to get boot plan: {}", e);
            log::error!("Failed to get boot plan: {}", e);
            None
        }
    }
}

async fn start_boot_plan_units(manager: &SharedManager, plan: &[String]) {
    for unit_name in plan {
        eprintln!("sysd: Starting {}", unit_name);
        log::info!("Starting {}", unit_name);
        let mut mgr = manager.write().await;
        match mgr.start(unit_name).await {
            Ok(()) => log::info!("Started {}", unit_name),
            Err(e) => {
                eprintln!("sysd: FAILED to start {}: {}", unit_name, e);
                log::warn!("Failed to start {}: {}", unit_name, e);
            }
        }
    }
}

async fn serve_requests(
    user_mode: bool,
    manager: SharedManager,
) -> Result<(), Box<dyn std::error::Error>> {
    let sock_path = socket_path(user_mode);
    let server = Server::bind(&sock_path)?;
    info!(
        "sysd{} listening on {}",
        if user_mode { " (user)" } else { "" },
        sock_path
    );
    loop {
        match server.accept().await {
            Ok((conn, caller)) => {
                let manager = Arc::clone(&manager);
                tokio::spawn(sysd_request_handlers::handle_connection(
                    conn, caller, manager,
                ));
            }
            Err(e) => log::error!("accept error: {}", e),
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

#[path = "sysd/request_handlers.rs"]
mod sysd_request_handlers;
