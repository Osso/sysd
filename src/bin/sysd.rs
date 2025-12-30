//! sysd - Minimal systemd-compatible init daemon
//!
//! Listens on /run/sysd.sock for commands from sysdctl.
//! Provides D-Bus interface at org.freedesktop.systemd1 for logind compatibility.
//!
//! When running as PID 1:
//! - Mounts essential filesystems
//! - Reaps zombie processes
//! - Handles signals for shutdown

use clap::Parser;
use log::info;
use std::sync::Arc;
use tokio::sync::RwLock;

use sysd::dbus::DbusServer;
use sysd::manager::Manager;
use sysd::pid1::{self, SignalHandler, SysdSignal, ShutdownType, ZombieReaper};
use sysd::protocol::{Request, Response, UnitInfo, SOCKET_PATH};
use peercred_ipc::{CallerInfo, Connection, Server};

#[derive(Parser)]
#[command(name = "sysd")]
#[command(about = "Minimal systemd-compatible init daemon")]
#[command(long_about = "sysd is a minimal init system that parses systemd unit files \
    and manages services. It listens on /run/sysd.sock for commands from sysdctl.")]
struct Args {
    /// Run in foreground (don't daemonize)
    #[arg(long, short = 'f')]
    foreground: bool,
}

/// Shared manager state accessible from IPC and D-Bus
type SharedManager = Arc<RwLock<Manager>>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _args = Args::parse();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let is_pid1 = pid1::is_pid1();

    // PID 1: Initialize essential filesystems and environment
    if is_pid1 {
        if let Err(e) = pid1::init() {
            log::error!("PID 1 initialization failed: {}", e);
            // Continue anyway - some mounts might have succeeded
        }
    }

    let mut manager = Manager::new();

    // Initialize notify socket for Type=notify services
    if let Err(e) = manager.init_notify_socket() {
        log::warn!("Failed to create notify socket: {} (Type=notify services won't work)", e);
    }

    let manager: SharedManager = Arc::new(RwLock::new(manager));

    // Start D-Bus server (shares manager with IPC)
    let _dbus_server = match DbusServer::new(Arc::clone(&manager)).await {
        Ok(server) => {
            info!("D-Bus interface available at org.freedesktop.systemd1");
            Some(server)
        }
        Err(e) => {
            log::warn!("Failed to start D-Bus server: {} (logind integration unavailable)", e);
            None
        }
    };

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

    // Spawn background task for processing notify messages and service reaping
    let manager_bg = Arc::clone(&manager);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            interval.tick().await;
            let mut mgr = manager_bg.write().await;
            mgr.process_notify().await;
            mgr.reap().await;
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
                        // TODO: reload unit files
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

    let server = Server::bind(SOCKET_PATH)?;
    info!("sysd listening on {}", SOCKET_PATH);

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

async fn handle_connection(
    mut conn: Connection,
    caller: CallerInfo,
    manager: SharedManager,
) {
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

        Request::List { user: _ } => {
            let mgr = manager.read().await;
            let units: Vec<UnitInfo> = mgr
                .list()
                .map(|(name, svc_state)| UnitInfo {
                    name: name.clone(),
                    unit_type: "service".into(), // TODO: detect type
                    state: format!("{:?}", svc_state.active),
                    description: None, // TODO: get from unit
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
                let mgr = manager.read().await;
                match mgr.get_default_target() {
                    Ok(target) => Response::BootPlan(vec![target]), // TODO: expand deps
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
            // TODO: implement reload
            Response::Ok
        }

        Request::SyncUnits => {
            // TODO: implement sync
            Response::Ok
        }
    }
}
