//! sysd - Minimal systemd-compatible init daemon
//!
//! Listens on /run/sysd.sock for commands from sysdctl.

use clap::Parser;
use log::info;
use std::sync::Arc;
use tokio::sync::RwLock;

use sysd::manager::Manager;
use sysd::protocol::{Request, Response, UnitInfo, SOCKET_PATH};
use unix_ipc::{CallerInfo, Connection, Server};

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

struct DaemonState {
    manager: Manager,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _args = Args::parse();

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let state = Arc::new(RwLock::new(DaemonState {
        manager: Manager::new(),
    }));

    let server = Server::bind(SOCKET_PATH)?;
    info!("sysd listening on {}", SOCKET_PATH);

    loop {
        match server.accept().await {
            Ok((conn, caller)) => {
                let state = Arc::clone(&state);
                tokio::spawn(handle_connection(conn, caller, state));
            }
            Err(e) => {
                log::error!("accept error: {}", e);
            }
        }
    }
}

async fn handle_connection(
    mut conn: Connection,
    caller: CallerInfo,
    state: Arc<RwLock<DaemonState>>,
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

    let response = handle_request(request, &state).await;
    if let Err(e) = conn.write(&response).await {
        log::error!("write error: {}", e);
    }
}

async fn handle_request(request: Request, state: &Arc<RwLock<DaemonState>>) -> Response {
    match request {
        Request::Ping => Response::Pong,

        Request::List { user: _ } => {
            let state = state.read().await;
            let units: Vec<UnitInfo> = state
                .manager
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
            let mut state = state.write().await;
            match state.manager.start(&name).await {
                Ok(_) => Response::Ok,
                Err(e) => Response::Error(e.to_string()),
            }
        }

        Request::Stop { name } => {
            let mut state = state.write().await;
            match state.manager.stop(&name).await {
                Ok(_) => Response::Ok,
                Err(e) => Response::Error(e.to_string()),
            }
        }

        Request::Restart { name } => {
            let mut state = state.write().await;
            match state.manager.restart(&name).await {
                Ok(_) => Response::Ok,
                Err(e) => Response::Error(e.to_string()),
            }
        }

        Request::Status { name } => {
            let state = state.read().await;
            match state.manager.status(&name) {
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
            let state = state.read().await;
            match state.manager.get_unit(&name) {
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

        Request::GetBootTarget => match state.read().await.manager.get_default_target() {
            Ok(target) => Response::BootTarget(target),
            Err(e) => Response::Error(e.to_string()),
        },

        Request::Boot { dry_run } => {
            if dry_run {
                let state = state.read().await;
                match state.manager.get_default_target() {
                    Ok(target) => Response::BootPlan(vec![target]), // TODO: expand deps
                    Err(e) => Response::Error(e.to_string()),
                }
            } else {
                let mut state = state.write().await;
                match state.manager.get_default_target() {
                    Ok(target) => match state.manager.start_with_deps(&target).await {
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
