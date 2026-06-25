// Runtime processing for the service manager.
//
// These functions run in the background task loop to handle:
// - sd_notify messages (READY, STOPPING, WATCHDOG)
// - Process reaping and restart scheduling
// - D-Bus name acquisition for Type=dbus services
// - Watchdog timeouts

use crate::units::{NotifyAccess, RestartPolicy, ServiceType};

use crate::manager::notify::NotifyMessage;
use crate::manager::process;
use crate::manager::state::{ActiveState, SubState};
use crate::manager::{Manager, ManagerError, OneshotCompletion, SpawnOptions};


impl Manager {
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

        match self.notify_access_for_service(&service_name) {
            NotifyAccess::None => {
                log::debug!(
                    "Rejecting notify from {} (PID {}) - NotifyAccess=none",
                    service_name,
                    msg.pid
                );
                false
            }
            NotifyAccess::Main => self.validate_main_notify_access(&service_name, msg.pid),
            NotifyAccess::Exec | NotifyAccess::All => true,
        }
    }

    fn notify_access_for_service(&self, service_name: &str) -> NotifyAccess {
        self.units
            .get(service_name)
            .and_then(|u| u.as_service())
            .map(|s| s.service.notify_access.clone())
            .unwrap_or_default()
    }

    fn validate_main_notify_access(&self, service_name: &str, pid: u32) -> bool {
        let main_pid = self.processes.get(service_name).and_then(|c| c.id());
        if main_pid == Some(pid) {
            return true;
        }
        log::debug!(
            "Rejecting notify from {} (PID {}) - NotifyAccess=main requires main PID {:?}",
            service_name,
            pid,
            main_pid
        );
        false
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
        let Some(service_name) = self.find_service_by_pid(msg.pid) else {
            log::warn!(
                "FDSTORE from unknown PID {}, closing {} FDs",
                msg.pid,
                msg.fds.len()
            );
            close_fds(&msg.fds);
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
            close_fds(&msg.fds);
            return;
        }

        let fd_name = msg.fdname().unwrap_or("stored").to_string();
        let store = self.fd_store.entry(service_name.clone()).or_default();
        store_fd_entries(store, &service_name, &fd_name, &msg.fds, max_fds);
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

    fn set_forking_running_state(&mut self, name: &str, child_pid: u32, source: &std::path::Path) {
        if let Some(state) = self.states.get_mut(name) {
            state.set_running(child_pid);
            self.active_jobs = self.active_jobs.saturating_sub(1);
            log::info!(
                "{} forked, main PID {} (from {})",
                name,
                child_pid,
                source.display()
            );
        }
        self.arm_watchdog(name);
    }

    fn read_forking_pid_file(&self, pid_file: &std::path::Path) -> Result<u32, String> {
        let content = std::fs::read_to_string(pid_file)
            .map_err(|e| format!("failed to read PIDFile {}: {}", pid_file.display(), e))?;
        content
            .trim()
            .parse::<u32>()
            .map_err(|_| format!("invalid PID in {}", pid_file.display()))
    }

    /// Handle Type=forking parent exit: read PIDFile and update state
    /// Returns true if handled (caller should skip normal exit processing)
    fn reap_forking_parent(&mut self, name: &str, code: i32) -> bool {
        if code != 0 {
            return false;
        }

        let Some(pid_file) = self.pid_files.remove(name) else {
            log::warn!("{} forked but no PIDFile configured", name);
            if let Some(state) = self.states.get_mut(name) {
                state.set_running(0);
            }
            self.active_jobs = self.active_jobs.saturating_sub(1);
            self.arm_watchdog(name);
            return true;
        };

        match self.read_forking_pid_file(&pid_file) {
            Ok(child_pid) => {
                self.set_forking_running_state(name, child_pid, &pid_file);
                true
            }
            Err(error) => {
                log::warn!("{}: {}", name, error);
                false
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
        let exited = self.collect_exited_services();
        for (name, code) in exited {
            self.handle_reaped_service(name, code).await;
        }
    }

    fn collect_exited_services(&mut self) -> Vec<(String, i32)> {
        let mut exited = Vec::new();
        while let Some(service_exit) = self.reap_next_service_exit() {
            exited.push(service_exit);
        }
        exited
    }

    fn reap_next_service_exit(&mut self) -> Option<(String, i32)> {
        use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
        use nix::unistd::Pid;

        loop {
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) | Err(nix::errno::Errno::ECHILD) => return None,
                Err(e) => {
                    log::error!("waitpid error: {}", e);
                    return None;
                }
                Ok(status) => {
                    if let Some(service_exit) = self.resolve_reaped_status(status) {
                        return Some(service_exit);
                    }
                }
            }
        }
    }

    fn resolve_reaped_status(
        &mut self,
        status: nix::sys::wait::WaitStatus,
    ) -> Option<(String, i32)> {
        let (pid, code) = Self::decode_wait_status(status)?;
        let service_name = self.pid_to_service.remove(&pid);
        if let Some(name) = service_name {
            log::debug!("Reaped {} (PID {}) with exit code {}", name, pid, code);
            return Some((name, code));
        }
        log::debug!("Reaped orphaned process PID {} (exit {})", pid, code);
        None
    }

    fn decode_wait_status(status: nix::sys::wait::WaitStatus) -> Option<(u32, i32)> {
        use nix::sys::wait::WaitStatus;
        match status {
            WaitStatus::Exited(p, code) => Some((p.as_raw() as u32, code)),
            WaitStatus::Signaled(p, signal, _) => Some((p.as_raw() as u32, -(signal as i32))),
            WaitStatus::Stopped(p, _) | WaitStatus::Continued(p) => {
                log::debug!("Process {} stopped/continued", p.as_raw());
                None
            }
            _ => None,
        }
    }

    async fn handle_reaped_service(&mut self, name: String, code: i32) {
        self.processes.remove(&name);
        let policy = self.read_restart_policy(&name);

        if policy.is_forking && self.reap_forking_parent(&name, code) {
            return;
        }

        self.apply_restart_decision(
            &name,
            code,
            policy.is_oneshot,
            policy.remain_after_exit,
            &policy.restart_policy,
            policy.restart_sec,
            policy.start_limit_burst,
            policy.start_limit_interval_sec,
            &policy.restart_prevent_exit_status,
        );
        self.cleanup_after_exit(&name).await;
    }

    fn read_restart_policy(&self, name: &str) -> RestartDecisionInput {
        self.units
            .get(name)
            .and_then(|u| u.as_service())
            .map(|s| RestartDecisionInput {
                restart_policy: s.service.restart.clone(),
                restart_sec: s.service.restart_sec,
                remain_after_exit: s.service.remain_after_exit,
                is_oneshot: s.service.service_type == ServiceType::Oneshot,
                is_forking: s.service.service_type == ServiceType::Forking,
                start_limit_burst: s.service.start_limit_burst,
                start_limit_interval_sec: s.service.start_limit_interval_sec,
                restart_prevent_exit_status: s.service.restart_prevent_exit_status.clone(),
            })
            .unwrap_or_default()
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

}
