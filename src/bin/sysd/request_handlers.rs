use log::info;
use peercred_ipc::{CallerInfo, Connection};

use super::SharedManager;
use sysd::protocol::{Request, Response, UnitInfo};

pub(super) async fn handle_connection(
    mut conn: Connection,
    caller: CallerInfo,
    manager: SharedManager,
) {
    info!(
        "connection from uid={} pid={} exe={:?}",
        caller.uid, caller.pid, caller.exe
    );
    let request: Request = match conn.read().await {
        Ok(request) => request,
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
                .map_or(true, |unit_type| unit.unit_type() == unit_type.as_str())
        })
        .map(|(name, unit, state)| UnitInfo {
            name: name.clone(),
            unit_type: unit.unit_type().into(),
            state: state
                .map(|state| format!("{:?}", state.active))
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
