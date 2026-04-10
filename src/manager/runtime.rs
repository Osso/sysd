//! Runtime processing for the service manager
//!
//! These functions run in the background task loop to handle:
//! - sd_notify messages (READY, STOPPING, WATCHDOG)
//! - Process reaping and restart scheduling
//! - D-Bus name acquisition for Type=dbus services
//! - Watchdog timeouts

use crate::units::{NotifyAccess, RestartPolicy, ServiceType};

use super::notify::NotifyMessage;
use super::state::{ActiveState, SubState};
use super::Manager;

impl Manager {
    /// Find which service a PID belongs to (returns service name if found)
    fn find_service_by_pid(&self, pid: u32) -> Option<String> {
        // Check waiting_ready first (for Type=notify services starting up)
        if let Some(name) = self.waiting_ready.get(&pid) {
            return Some(name.clone());
        }

        // Check running processes
        for (name, child) in &self.processes {
            if child.id() == Some(pid) {
                return Some(name.clone());
            }
        }

        None
    }

    /// Validate if a notify message should be accepted based on NotifyAccess policy
    fn validate_notify_access(&self, msg: &NotifyMessage) -> bool {
        let Some(service_name) = self.find_service_by_pid(msg.pid) else {
            log::debug!(
                "Notify message from unknown PID {}, accepting for now",
                msg.pid
            );
            return true;
        };

        let notify_access = self
            .units
            .get(&service_name)
            .and_then(|u| u.as_service())
            .map(|s| &s.service.notify_access)
            .cloned()
            .unwrap_or_default();

        match notify_access {
            NotifyAccess::None => {
                log::debug!(
                    "Rejecting notify from {} (PID {}) - NotifyAccess=none",
                    service_name,
                    msg.pid
                );
                false
            }
            NotifyAccess::Main => {
                let main_pid = self.processes.get(&service_name).and_then(|c| c.id());
                if main_pid == Some(msg.pid) {
                    true
                } else {
                    log::debug!(
                        "Rejecting notify from {} (PID {}) - NotifyAccess=main requires main PID {:?}",
                        service_name,
                        msg.pid,
                        main_pid
                    );
                    false
                }
            }
            NotifyAccess::Exec | NotifyAccess::All => true,
        }
    }

    /// Resolve which service name to mark ready, consuming the entry from waiting_ready
    fn resolve_ready_service_name(&mut self, msg: &NotifyMessage) -> Option<String> {
        if let Some(main_pid) = msg.main_pid() {
            if let Some(name) = self.waiting_ready.remove(&main_pid) {
                return Some(name);
            }
            return self.waiting_ready.remove(&msg.pid);
        }

        if let Some(name) = self.waiting_ready.remove(&msg.pid) {
            return Some(name);
        }

        // Try to find by iterating processes
        let found = self.processes.iter().find_map(|(name, child)| {
            child
                .id()
                .filter(|pid| self.waiting_ready.contains_key(pid))
                .map(|pid| (pid, name.clone()))
        });
        if let Some((pid, name)) = found {
            self.waiting_ready.remove(&pid);
            Some(name)
        } else {
            None
        }
    }

    /// Mark a service as ready and optionally arm its watchdog
    fn mark_service_ready(&mut self, name: &str) {
        if let Some(state) = self.states.get_mut(name) {
            let pid = self.processes.get(name).and_then(|c| c.id()).unwrap_or(0);
            state.set_running(pid);
            self.active_jobs = self.active_jobs.saturating_sub(1);
            log::info!("{} signaled READY", name);
        }
        self.arm_watchdog(name);
    }

    /// Set or reset the watchdog deadline for a service if WatchdogSec is configured
    fn arm_watchdog(&mut self, name: &str) {
        if let Some(wd) = self
            .units
            .get(name)
            .and_then(|u| u.as_service())
            .and_then(|s| s.service.watchdog_sec)
        {
            self.watchdog_deadlines
                .insert(name.to_string(), std::time::Instant::now() + wd);
        }
    }

    fn handle_notify_ready(&mut self, msg: &NotifyMessage) {
        log::debug!(
            "READY received: sender_pid={}, main_pid={:?}, waiting_ready={:?}",
            msg.pid,
            msg.main_pid(),
            self.waiting_ready
        );

        if let Some(name) = self.resolve_ready_service_name(msg) {
            self.mark_service_ready(&name);
        } else {
            log::warn!(
                "READY message from PID {} (main_pid={:?}) could not be matched to any service",
                msg.pid,
                msg.main_pid()
            );
        }
    }

    fn handle_notify_watchdog_ping(&mut self, msg: &NotifyMessage) {
        let service_name = self
            .processes
            .iter()
            .find(|(_, child)| child.id() == Some(msg.pid))
            .map(|(name, _)| name.clone());

        if let Some(name) = service_name {
            if self
                .units
                .get(&name)
                .and_then(|u| u.as_service())
                .and_then(|s| s.service.watchdog_sec)
                .is_some()
            {
                self.arm_watchdog(&name);
                log::trace!("{} watchdog ping received", name);
            }
        }
    }

    /// Process pending notify messages (READY, STOPPING, WATCHDOG, etc.)
    pub async fn process_notify(&mut self) {
        let messages: Vec<_> = {
            let Some(rx) = &mut self.notify_rx else {
                return;
            };
            std::iter::from_fn(|| rx.try_recv().ok()).collect()
        };

        for msg in messages {
            log::debug!("Notify message from PID {}: {:?}", msg.pid, msg.fields);
            if self.validate_notify_access(&msg) {
                self.dispatch_notify(&msg);
            }
        }
    }

    fn dispatch_notify(&mut self, msg: &NotifyMessage) {
        if msg.is_ready() {
            self.handle_notify_ready(msg);
        }
        if msg.is_watchdog() {
            self.handle_notify_watchdog_ping(msg);
        }
        if msg.is_stopping() {
            log::debug!("Service signaled STOPPING (PID hint: {})", msg.pid);
        }
        if msg.is_fdstore() && !msg.fds.is_empty() {
            self.handle_fdstore(msg);
        }
        if msg.is_fdstoreremove() {
            self.handle_fdstoreremove(msg);
        }
        if let Some(status) = msg.status() {
            log::debug!("Service status: {}", status);
        }
    }

    /// M19: Handle FDSTORE=1 notification - store FDs for service restart
    fn handle_fdstore(&mut self, msg: &NotifyMessage) {
        let service_name = self.find_service_by_pid(msg.pid);
        let Some(service_name) = service_name else {
            log::warn!(
                "FDSTORE from unknown PID {}, closing {} FDs",
                msg.pid,
                msg.fds.len()
            );
            for fd in &msg.fds {
                unsafe { libc::close(*fd) };
            }
            return;
        };

        let max_fds = self
            .units
            .get(&service_name)
            .and_then(|u| u.as_service())
            .and_then(|s| s.service.file_descriptor_store_max)
            .unwrap_or(0) as usize;

        if max_fds == 0 {
            log::warn!(
                "{}: FDSTORE received but FileDescriptorStoreMax=0, closing {} FDs",
                service_name,
                msg.fds.len()
            );
            for fd in &msg.fds {
                unsafe { libc::close(*fd) };
            }
            return;
        }

        let fd_name = msg.fdname().unwrap_or("stored").to_string();
        let store = self.fd_store.entry(service_name.clone()).or_default();

        for fd in &msg.fds {
            if store.len() >= max_fds {
                log::warn!(
                    "{}: FD store full (max {}), closing extra FD",
                    service_name,
                    max_fds
                );
                unsafe { libc::close(*fd) };
            } else {
                log::debug!("{}: Storing FD {} as '{}'", service_name, fd, fd_name);
                store.push((fd_name.clone(), *fd));
            }
        }
    }

    /// M19: Handle FDSTOREREMOVE=1 notification - remove stored FDs by name
    fn handle_fdstoreremove(&mut self, msg: &NotifyMessage) {
        let service_name = self.find_service_by_pid(msg.pid);
        let Some(service_name) = service_name else {
            return;
        };

        let Some(fd_name) = msg.fdname() else {
            return;
        };

        if let Some(store) = self.fd_store.get_mut(&service_name) {
            store.retain(|(name, fd)| {
                if name == fd_name {
                    log::debug!("{}: Removing stored FD {} ('{}')", service_name, fd, name);
                    unsafe { libc::close(*fd) };
                    false
                } else {
                    true
                }
            });
        }
    }

    /// Handle Type=forking parent exit: read PIDFile and update state
    /// Returns true if handled (caller should skip normal exit processing)
    fn reap_forking_parent(&mut self, name: &str, code: i32) -> bool {
        if code != 0 {
            return false;
        }

        match self.pid_files.remove(name) {
            Some(pid_file) => match std::fs::read_to_string(&pid_file) {
                Ok(content) => match content.trim().parse::<u32>() {
                    Ok(child_pid) => {
                        if let Some(state) = self.states.get_mut(name) {
                            state.set_running(child_pid);
                            self.active_jobs = self.active_jobs.saturating_sub(1);
                            log::info!(
                                "{} forked, main PID {} (from {})",
                                name,
                                child_pid,
                                pid_file.display()
                            );
                        }
                        self.arm_watchdog(name);
                        true
                    }
                    Err(_) => {
                        log::warn!("{}: invalid PID in {}", name, pid_file.display());
                        false
                    }
                },
                Err(e) => {
                    log::warn!(
                        "{}: failed to read PIDFile {}: {}",
                        name,
                        pid_file.display(),
                        e
                    );
                    false
                }
            },
            None => {
                log::warn!("{} forked but no PIDFile configured", name);
                if let Some(state) = self.states.get_mut(name) {
                    state.set_running(0);
                }
                self.active_jobs = self.active_jobs.saturating_sub(1);
                self.arm_watchdog(name);
                true
            }
        }
    }

    /// Apply restart/stop/fail state transitions after a process exits
    fn apply_restart_decision(
        &mut self,
        name: &str,
        code: i32,
        is_oneshot: bool,
        remain_after_exit: bool,
        restart_policy: &RestartPolicy,
        restart_sec: std::time::Duration,
        start_limit_burst: Option<u32>,
        start_limit_interval_sec: Option<std::time::Duration>,
        restart_prevent_exit_status: &[i32],
    ) {
        let policy_wants_restart = match restart_policy {
            RestartPolicy::No => false,
            RestartPolicy::OnFailure => code != 0,
            RestartPolicy::Always => true,
        };
        let exit_prevents_restart = restart_prevent_exit_status.contains(&code);

        let Some(state) = self.states.get_mut(name) else {
            return;
        };

        let rate_limited =
            state.is_restart_rate_limited(start_limit_burst, start_limit_interval_sec);
        let should_restart = policy_wants_restart && !exit_prevents_restart && !rate_limited;

        if code == 0 {
            if is_oneshot && remain_after_exit {
                state.active = ActiveState::Active;
                state.sub = SubState::Exited;
                state.main_pid = None;
                state.exit_code = Some(code);
                state.reset_restart_count();
                log::info!("{} exited (RemainAfterExit=yes)", name);
            } else if should_restart {
                state.set_auto_restart(restart_sec);
                log::info!("{} exited, scheduling restart in {:?}", name, restart_sec);
            } else {
                state.set_stopped(code);
                state.reset_restart_count();
                log::info!("{} exited cleanly", name);
            }
        } else if rate_limited {
            state.set_failed(format!(
                "Start limit hit (burst {} in {:?})",
                start_limit_burst.unwrap_or(0),
                start_limit_interval_sec.unwrap_or(std::time::Duration::from_secs(10))
            ));
            log::error!("{} start limit hit, not restarting (exit {})", name, code);
        } else if exit_prevents_restart {
            state.set_stopped(code);
            log::info!(
                "{} exit code {} in RestartPreventExitStatus, not restarting",
                name,
                code
            );
        } else if should_restart {
            state.set_auto_restart(restart_sec);
            log::warn!(
                "{} failed (exit {}), scheduling restart in {:?}",
                name,
                code,
                restart_sec
            );
        } else {
            state.set_failed(format!("Exit code {}", code));
            log::warn!("{} failed with exit code {}", name, code);
        }
    }

    /// Clean up cgroup, watchdog, dynamic UID, and stored FDs after a service exits
    async fn cleanup_after_exit(&mut self, name: &str) {
        self.cleanup_service_cgroup(name);
        self.watchdog_deadlines.remove(name);

        let is_restarting = self
            .states
            .get(name)
            .is_some_and(|s| s.sub == SubState::AutoRestart);
        if !is_restarting {
            self.release_dynamic_uid(name);
            self.close_stored_fds(name);
        }

        self.propagate_binds_to_stop(name).await;
    }

    fn cleanup_service_cgroup(&mut self, name: &str) {
        if self.cgroup_paths.remove(name).is_none() {
            return;
        }
        let slice = self
            .units
            .get(name)
            .and_then(|u| u.as_service())
            .and_then(|s| s.service.slice.as_deref());
        if let Some(ref cgroup_mgr) = self.cgroup_manager {
            if let Err(e) = cgroup_mgr.cleanup_service_cgroup(name, slice) {
                log::debug!("Failed to clean up cgroup for {}: {}", name, e);
            }
        }
    }

    fn release_dynamic_uid(&mut self, name: &str) {
        if let Some(uid) = self.dynamic_uids.remove(name) {
            self.dynamic_user_manager.release(uid);
            log::debug!("Released dynamic UID {} for {}", uid, name);
        }
    }

    fn close_stored_fds(&mut self, name: &str) {
        if let Some(fds) = self.fd_store.remove(name) {
            for (fd_name, fd) in fds {
                log::debug!("Closing stored FD {} ('{}') for {}", fd, fd_name, name);
                unsafe { libc::close(fd) };
            }
        }
    }

    /// Check on running processes and update states
    ///
    /// Uses waitpid(-1, WNOHANG) to reap any zombie processes, then looks up
    /// which service they belong to. This approach:
    /// 1. Avoids race conditions with a separate zombie reaper
    /// 2. Preserves actual exit codes
    /// 3. Handles orphaned processes (reparented to PID 1)
    pub async fn reap(&mut self) {
        use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
        use nix::unistd::Pid;

        let mut exited = Vec::new();

        loop {
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => break,
                Ok(status) => {
                    let (pid, code) = match status {
                        WaitStatus::Exited(p, code) => (p.as_raw() as u32, code),
                        WaitStatus::Signaled(p, signal, _) => (p.as_raw() as u32, -(signal as i32)),
                        WaitStatus::Stopped(p, _) | WaitStatus::Continued(p) => {
                            log::debug!("Process {} stopped/continued", p.as_raw());
                            continue;
                        }
                        _ => continue,
                    };

                    if let Some(name) = self.pid_to_service.remove(&pid) {
                        log::debug!("Reaped {} (PID {}) with exit code {}", name, pid, code);
                        exited.push((name, code));
                    } else {
                        log::debug!("Reaped orphaned process PID {} (exit {})", pid, code);
                    }
                }
                Err(nix::errno::Errno::ECHILD) => break,
                Err(e) => {
                    log::error!("waitpid error: {}", e);
                    break;
                }
            }
        }

        for (name, code) in exited {
            self.processes.remove(&name);

            let (
                restart_policy,
                restart_sec,
                remain_after_exit,
                is_oneshot,
                is_forking,
                start_limit_burst,
                start_limit_interval_sec,
                restart_prevent_exit_status,
            ) = self
                .units
                .get(&name)
                .and_then(|u| u.as_service())
                .map(|s| {
                    (
                        s.service.restart.clone(),
                        s.service.restart_sec,
                        s.service.remain_after_exit,
                        s.service.service_type == ServiceType::Oneshot,
                        s.service.service_type == ServiceType::Forking,
                        s.service.start_limit_burst,
                        s.service.start_limit_interval_sec,
                        s.service.restart_prevent_exit_status.clone(),
                    )
                })
                .unwrap_or((
                    RestartPolicy::No,
                    std::time::Duration::from_millis(100),
                    false,
                    false,
                    false,
                    None,
                    None,
                    Vec::new(),
                ));

            if is_forking && self.reap_forking_parent(&name, code) {
                continue;
            }

            self.apply_restart_decision(
                &name,
                code,
                is_oneshot,
                remain_after_exit,
                &restart_policy,
                restart_sec,
                start_limit_burst,
                start_limit_interval_sec,
                &restart_prevent_exit_status,
            );

            self.cleanup_after_exit(&name).await;
        }
    }

    /// M19: BindsTo= stop propagation
    /// When a unit stops, find all units with BindsTo= pointing to it and stop them
    async fn propagate_binds_to_stop(&mut self, stopped_unit: &str) {
        let units_to_stop: Vec<String> = self
            .units
            .iter()
            .filter_map(|(name, unit)| {
                let binds_to = &unit.unit_section().binds_to;
                if binds_to.contains(&stopped_unit.to_string()) {
                    if let Some(state) = self.states.get(name) {
                        if state.is_active() {
                            return Some(name.clone());
                        }
                    }
                }
                None
            })
            .collect();

        for name in units_to_stop {
            log::info!("Stopping {} (BindsTo={} which stopped)", name, stopped_unit);
            if let Err(e) = self.stop(&name).await {
                log::warn!(
                    "Failed to stop {} after BindsTo dependency stopped: {}",
                    name,
                    e
                );
            }
        }
    }

    /// Process pending restarts
    pub async fn process_restarts(&mut self) {
        let due: Vec<String> = self
            .states
            .iter()
            .filter(|(_, state)| state.sub == SubState::AutoRestart && state.restart_due())
            .map(|(name, _)| name.clone())
            .collect();

        for name in due {
            log::info!("Restarting {}", name);
            if let Some(state) = self.states.get_mut(&name) {
                state.clear_restart();
            }
            if let Err(e) = self.start_single(&name).await {
                log::error!("Failed to restart {}: {}", name, e);
                if let Some(state) = self.states.get_mut(&name) {
                    state.set_failed(format!("Restart failed: {}", e));
                }
            }
        }
    }

    /// Take the oneshot completion receiver (for use in background task)
    pub fn take_oneshot_completion_rx(
        &mut self,
    ) -> Option<tokio::sync::mpsc::Receiver<super::OneshotCompletion>> {
        self.oneshot_completion_rx.take()
    }

    /// Remove the cgroup for a service, logging results
    fn cleanup_oneshot_cgroup(&mut self, service_name: &str) {
        if let Some(cgroup_path) = self.cgroup_paths.remove(service_name) {
            if let Some(ref cgroup_mgr) = self.cgroup_manager {
                if let Err(e) = cgroup_mgr.remove_cgroup(&cgroup_path) {
                    log::warn!("Failed to remove cgroup for {}: {}", service_name, e);
                }
                log::info!("Removed cgroup: {}", cgroup_path.display());
            }
        }
    }

    /// Fail a oneshot service, cleaning up state and cgroup
    fn fail_oneshot(&mut self, service_name: &str, error: &str) {
        self.cleanup_oneshot_cgroup(service_name);
        self.active_jobs = self.active_jobs.saturating_sub(1);
        if let Some(state) = self.states.get_mut(service_name) {
            state.set_failed(error.to_string());
        }
        self.pending_oneshot_cmds.remove(service_name);
    }

    /// Process oneshot completion messages
    pub async fn handle_oneshot_completion(&mut self, completion: super::OneshotCompletion) {
        let service_name = &completion.service_name;
        log::debug!(
            "Oneshot {} command {} completed (exit={:?}, error={:?})",
            service_name,
            completion.cmd_idx,
            completion.exit_code,
            completion.error
        );

        if let Some(ref error) = completion.error {
            let error = error.clone();
            let name = service_name.clone();
            self.fail_oneshot(&name, &error);
            log::warn!("Oneshot {} failed: {}", name, error);
            return;
        }

        let next_idx = completion.cmd_idx + 1;

        if next_idx < completion.total_cmds {
            log::debug!(
                "Oneshot {} starting command {} of {}",
                service_name,
                next_idx + 1,
                completion.total_cmds
            );

            self.pending_oneshot_cmds.insert(
                service_name.clone(),
                (
                    next_idx,
                    completion.total_cmds,
                    completion.remain_after_exit,
                ),
            );

            let name = service_name.clone();
            if let Err(e) = self.start_oneshot_command(&name, next_idx).await {
                let msg = format!("Command {} failed: {}", next_idx, e);
                self.fail_oneshot(&name, &msg);
                log::warn!(
                    "Oneshot {} command {} failed to start: {}",
                    name,
                    next_idx,
                    e
                );
            }
        } else {
            let name = service_name.clone();
            self.cleanup_oneshot_cgroup(&name);
            self.active_jobs = self.active_jobs.saturating_sub(1);
            if let Some(state) = self.states.get_mut(&name) {
                if completion.remain_after_exit {
                    state.set_exited();
                } else {
                    state.set_inactive();
                }
            }
            self.pending_oneshot_cmds.remove(&name);
            log::info!("Oneshot {} completed successfully (exit 0)", name);
        }
    }

    /// Start a specific command of a oneshot service (used for multi-command oneshots)
    async fn start_oneshot_command(
        &mut self,
        service_name: &str,
        cmd_idx: usize,
    ) -> Result<(), super::ManagerError> {
        let service = self
            .units
            .get(service_name)
            .and_then(|u| u.as_service())
            .ok_or_else(|| super::ManagerError::NotFound(service_name.to_string()))?
            .clone();

        let total_cmds = service.service.exec_start.len();
        let remain_after_exit = service.service.remain_after_exit;

        let options = super::SpawnOptions::default();

        let child = super::process::spawn_service_via_executor(
            &service,
            &options,
            &self.executor_path,
            cmd_idx,
        )?;

        let pid = child.id().unwrap_or(0);
        log::info!(
            "Oneshot {} command {} started (PID {})",
            service_name,
            cmd_idx,
            pid
        );

        if let Some(ref cgroup_path) = self.cgroup_paths.get(service_name) {
            if let Some(ref cgroup_mgr) = self.cgroup_manager {
                if let Err(e) = cgroup_mgr.add_pid(cgroup_path, pid) {
                    log::warn!(
                        "Failed to add PID {} to cgroup for {}: {}",
                        pid,
                        service_name,
                        e
                    );
                }
            }
        }

        let service_name_clone = service_name.to_string();
        let tx = self.oneshot_completion_tx.clone();

        tokio::spawn(async move {
            let result = child.wait_with_output().await;
            let (exit_code, error) = match result {
                Ok(output) => {
                    let code = output.status.code().unwrap_or(-1);
                    if code == 0 {
                        (Some(0), None)
                    } else {
                        (Some(code), Some(format!("exit code {}", code)))
                    }
                }
                Err(e) => (None, Some(e.to_string())),
            };

            let _ = tx
                .send(super::OneshotCompletion {
                    service_name: service_name_clone,
                    cmd_idx,
                    total_cmds,
                    exit_code,
                    error,
                    remain_after_exit,
                })
                .await;
        });

        Ok(())
    }

    /// Check if any Type=dbus services have acquired their bus name
    pub async fn process_dbus_ready(&mut self) {
        if self.waiting_bus_name.is_empty() {
            return;
        }

        let conn = match zbus::Connection::system().await {
            Ok(c) => c,
            Err(e) => {
                log::debug!("Cannot check D-Bus names (no connection): {}", e);
                return;
            }
        };

        let mut ready = Vec::new();
        for (bus_name, service_name) in &self.waiting_bus_name {
            match conn
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "GetNameOwner",
                    &(bus_name.as_str(),),
                )
                .await
            {
                Ok(_) => {
                    ready.push((bus_name.clone(), service_name.clone()));
                }
                Err(_) => {}
            }
        }

        for (bus_name, service_name) in ready {
            self.waiting_bus_name.remove(&bus_name);
            if let Some(state) = self.states.get_mut(&service_name) {
                let pid = self
                    .processes
                    .get(&service_name)
                    .and_then(|c| c.id())
                    .unwrap_or(0);
                state.set_running(pid);
                self.active_jobs = self.active_jobs.saturating_sub(1);
                log::info!("{} acquired D-Bus name {}", service_name, bus_name);
            }
            self.arm_watchdog(&service_name);
        }
    }

    /// Check for watchdog timeouts and restart services that missed their deadline
    pub async fn process_watchdog(&mut self) {
        let now = std::time::Instant::now();
        let timed_out: Vec<String> = self
            .watchdog_deadlines
            .iter()
            .filter(|(_, deadline)| now > **deadline)
            .map(|(name, _)| name.clone())
            .collect();

        for name in timed_out {
            self.watchdog_deadlines.remove(&name);
            log::warn!("{} watchdog timeout - restarting", name);

            if let Some(mut child) = self.processes.remove(&name) {
                if let Some(pid) = child.id() {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGABRT);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let _ = child.kill().await;
                }
            }

            if let Some(state) = self.states.get_mut(&name) {
                state.set_failed("Watchdog timeout".to_string());
            }

            let restart_policy = self
                .units
                .get(&name)
                .and_then(|u| u.as_service())
                .map(|s| s.service.restart.clone())
                .unwrap_or(RestartPolicy::No);

            if restart_policy == RestartPolicy::Always || restart_policy == RestartPolicy::OnFailure
            {
                let restart_sec = self
                    .units
                    .get(&name)
                    .and_then(|u| u.as_service())
                    .map(|s| s.service.restart_sec)
                    .unwrap_or(std::time::Duration::from_millis(100));

                if let Some(state) = self.states.get_mut(&name) {
                    state.set_auto_restart(restart_sec);
                    log::info!("{} scheduling watchdog restart in {:?}", name, restart_sec);
                }
            }
        }
    }
}
