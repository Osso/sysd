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
        // Find the service for this PID
        let Some(service_name) = self.find_service_by_pid(msg.pid) else {
            // Unknown PID - could be from a child process
            // For now, accept it and let the message processing handle matching
            log::debug!(
                "Notify message from unknown PID {}, accepting for now",
                msg.pid
            );
            return true;
        };

        // Get the service's NotifyAccess policy
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
                // Only accept from the main process
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
            NotifyAccess::Exec | NotifyAccess::All => {
                // Accept from any process in the cgroup (we approximate by accepting all)
                true
            }
        }
    }

    /// Process pending notify messages (READY, STOPPING, WATCHDOG, etc.)
    pub async fn process_notify(&mut self) {
        // Collect all pending messages first to avoid borrow conflicts
        let messages: Vec<_> = {
            let Some(rx) = &mut self.notify_rx else {
                return;
            };
            std::iter::from_fn(|| rx.try_recv().ok()).collect()
        };

        // Process collected messages
        for msg in messages {
            // Validate NotifyAccess policy before processing
            if !self.validate_notify_access(&msg) {
                continue;
            }

            if msg.is_ready() {
                // Find which service this PID belongs to
                // First check waiting_ready map, then fall back to process map
                let service_name = if let Some(pid) = msg.main_pid() {
                    self.waiting_ready.remove(&pid)
                } else {
                    // Try to find by iterating processes
                    let mut found = None;
                    for (name, child) in &self.processes {
                        if let Some(pid) = child.id() {
                            if self.waiting_ready.contains_key(&pid) {
                                found = Some((pid, name.clone()));
                                break;
                            }
                        }
                    }
                    if let Some((pid, name)) = found {
                        self.waiting_ready.remove(&pid);
                        Some(name)
                    } else {
                        None
                    }
                };

                if let Some(name) = service_name {
                    if let Some(state) = self.states.get_mut(&name) {
                        let pid = self.processes.get(&name).and_then(|c| c.id()).unwrap_or(0);
                        state.set_running(pid);
                        self.active_jobs = self.active_jobs.saturating_sub(1);
                        log::info!("{} signaled READY", name);

                        // Set watchdog deadline for Type=notify services
                        if let Some(wd) = self
                            .units
                            .get(&name)
                            .and_then(|u| u.as_service())
                            .and_then(|s| s.service.watchdog_sec)
                        {
                            self.watchdog_deadlines
                                .insert(name.clone(), std::time::Instant::now() + wd);
                        }
                    }
                }
            }

            // Handle WATCHDOG=1 ping - reset the deadline
            if msg.is_watchdog() {
                // Find service by PID
                let service_name = self
                    .processes
                    .iter()
                    .find(|(_, child)| child.id() == Some(msg.pid))
                    .map(|(name, _)| name.clone());

                if let Some(name) = service_name {
                    if let Some(wd) = self
                        .units
                        .get(&name)
                        .and_then(|u| u.as_service())
                        .and_then(|s| s.service.watchdog_sec)
                    {
                        self.watchdog_deadlines
                            .insert(name.clone(), std::time::Instant::now() + wd);
                        log::trace!("{} watchdog ping received", name);
                    }
                }
            }

            if msg.is_stopping() {
                log::debug!("Service signaled STOPPING (PID hint: {})", msg.pid);
            }

            // M19: Handle FDSTORE=1 - store file descriptors for restart
            if msg.is_fdstore() && !msg.fds.is_empty() {
                self.handle_fdstore(&msg);
            }

            // M19: Handle FDSTOREREMOVE=1 - remove stored file descriptors
            if msg.is_fdstoreremove() {
                self.handle_fdstoreremove(&msg);
            }

            if let Some(status) = msg.status() {
                log::debug!("Service status: {}", status);
            }
        }
    }

    /// M19: Handle FDSTORE=1 notification - store FDs for service restart
    fn handle_fdstore(&mut self, msg: &NotifyMessage) {
        // Find the service for this PID
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

        // Check FileDescriptorStoreMax limit
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

        // Store FDs up to the limit
        for fd in &msg.fds {
            if store.len() >= max_fds {
                log::warn!(
                    "{}: FD store full (max {}), closing extra FD",
                    service_name,
                    max_fds
                );
                unsafe { libc::close(*fd) };
            } else {
                log::debug!(
                    "{}: Storing FD {} as '{}'",
                    service_name,
                    fd,
                    fd_name
                );
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

    /// Check on running processes and update states
    pub async fn reap(&mut self) {
        let mut exited = Vec::new();

        for (name, child) in &mut self.processes {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exited.push((name.clone(), status.code().unwrap_or(-1)));
                }
                Ok(None) => {
                    // Still running
                }
                Err(e) => {
                    log::error!("Error checking {}: {}", name, e);
                }
            }
        }

        for (name, code) in exited {
            self.processes.remove(&name);

            // Get service config for restart policy
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

            // Handle Type=forking: parent exited, read PIDFile
            if is_forking && code == 0 {
                if let Some(pid_file) = self.pid_files.remove(&name) {
                    match std::fs::read_to_string(&pid_file) {
                        Ok(content) => {
                            if let Ok(child_pid) = content.trim().parse::<u32>() {
                                if let Some(state) = self.states.get_mut(&name) {
                                    state.set_running(child_pid);
                                    self.active_jobs = self.active_jobs.saturating_sub(1);
                                    log::info!(
                                        "{} forked, main PID {} (from {})",
                                        name,
                                        child_pid,
                                        pid_file.display()
                                    );
                                }
                                // Set watchdog deadline for Type=forking services
                                if let Some(wd) = self
                                    .units
                                    .get(&name)
                                    .and_then(|u| u.as_service())
                                    .and_then(|s| s.service.watchdog_sec)
                                {
                                    self.watchdog_deadlines
                                        .insert(name.clone(), std::time::Instant::now() + wd);
                                }
                                continue; // Don't process as normal exit
                            } else {
                                log::warn!("{}: invalid PID in {}", name, pid_file.display());
                            }
                        }
                        Err(e) => {
                            log::warn!(
                                "{}: failed to read PIDFile {}: {}",
                                name,
                                pid_file.display(),
                                e
                            );
                        }
                    }
                } else {
                    // No PIDFile - assume forked successfully, but we can't track the child
                    log::warn!("{} forked but no PIDFile configured", name);
                    if let Some(state) = self.states.get_mut(&name) {
                        state.set_running(0); // Unknown PID
                    }
                    self.active_jobs = self.active_jobs.saturating_sub(1);
                    // Set watchdog deadline even without PIDFile
                    if let Some(wd) = self
                        .units
                        .get(&name)
                        .and_then(|u| u.as_service())
                        .and_then(|s| s.service.watchdog_sec)
                    {
                        self.watchdog_deadlines
                            .insert(name.clone(), std::time::Instant::now() + wd);
                    }
                    continue;
                }
            }

            // Determine if we should restart (base policy)
            let policy_wants_restart = match restart_policy {
                RestartPolicy::No => false,
                RestartPolicy::OnFailure => code != 0,
                RestartPolicy::Always => true,
            };

            // Check if this exit code prevents restart (RestartPreventExitStatus=)
            let exit_prevents_restart = restart_prevent_exit_status.contains(&code);

            if let Some(state) = self.states.get_mut(&name) {
                // Check rate limiting before deciding to restart
                let rate_limited =
                    state.is_restart_rate_limited(start_limit_burst, start_limit_interval_sec);

                // Final decision: policy wants restart AND not prevented by exit status AND not rate limited
                let should_restart = policy_wants_restart && !exit_prevents_restart && !rate_limited;

                if code == 0 {
                    // Clean exit
                    if is_oneshot && remain_after_exit {
                        // Keep as active (exited) for oneshot with RemainAfterExit=yes
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
                } else {
                    // Failed exit
                    if rate_limited {
                        state.set_failed(format!(
                            "Start limit hit (burst {} in {:?})",
                            start_limit_burst.unwrap_or(0),
                            start_limit_interval_sec.unwrap_or(std::time::Duration::from_secs(10))
                        ));
                        log::error!(
                            "{} start limit hit, not restarting (exit {})",
                            name,
                            code
                        );
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
            }

            // Clean up cgroup
            if self.cgroup_paths.remove(&name).is_some() {
                // Get slice from service config for cleanup
                let slice = self
                    .units
                    .get(&name)
                    .and_then(|u| u.as_service())
                    .and_then(|s| s.service.slice.as_deref());
                if let Some(ref cgroup_mgr) = self.cgroup_manager {
                    if let Err(e) = cgroup_mgr.cleanup_service_cgroup(&name, slice) {
                        log::debug!("Failed to clean up cgroup for {}: {}", name, e);
                    }
                }
            }

            // Clean up watchdog (will be re-set on restart)
            self.watchdog_deadlines.remove(&name);

            // M19: Release dynamic UID and stored FDs if allocated (and not restarting)
            if let Some(state) = self.states.get(&name) {
                if state.sub != SubState::AutoRestart {
                    // Release dynamic UID
                    if let Some(uid) = self.dynamic_uids.remove(&name) {
                        self.dynamic_user_manager.release(uid);
                        log::debug!("Released dynamic UID {} for {}", uid, name);
                    }
                    // Close and remove stored FDs
                    if let Some(fds) = self.fd_store.remove(&name) {
                        for (fd_name, fd) in fds {
                            log::debug!("Closing stored FD {} ('{}') for {}", fd, fd_name, name);
                            unsafe { libc::close(fd) };
                        }
                    }
                }
            }

            // M19: BindsTo= - stop units that have BindsTo= pointing to this unit
            self.propagate_binds_to_stop(&name).await;
        }
    }

    /// M19: BindsTo= stop propagation
    /// When a unit stops, find all units with BindsTo= pointing to it and stop them
    async fn propagate_binds_to_stop(&mut self, stopped_unit: &str) {
        // Collect units that need to be stopped
        let units_to_stop: Vec<String> = self
            .units
            .iter()
            .filter_map(|(name, unit)| {
                let binds_to = &unit.unit_section().binds_to;
                if binds_to.contains(&stopped_unit.to_string()) {
                    // Check if this unit is currently active
                    if let Some(state) = self.states.get(name) {
                        if state.is_active() {
                            return Some(name.clone());
                        }
                    }
                }
                None
            })
            .collect();

        // Stop each bound unit
        for name in units_to_stop {
            log::info!(
                "Stopping {} (BindsTo={} which stopped)",
                name,
                stopped_unit
            );
            if let Err(e) = self.stop(&name).await {
                log::warn!("Failed to stop {} after BindsTo dependency stopped: {}", name, e);
            }
        }
    }

    /// Process pending restarts
    pub async fn process_restarts(&mut self) {
        // Collect services due for restart
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

    /// Check if any Type=dbus services have acquired their bus name
    pub async fn process_dbus_ready(&mut self) {
        if self.waiting_bus_name.is_empty() {
            return;
        }

        // Try to connect to system bus
        let conn = match zbus::Connection::system().await {
            Ok(c) => c,
            Err(e) => {
                log::debug!("Cannot check D-Bus names (no connection): {}", e);
                return;
            }
        };

        // Check each waited name
        let mut ready = Vec::new();
        for (bus_name, service_name) in &self.waiting_bus_name {
            // Use the fdo DBus interface to check if the name has an owner
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
                    // Name has an owner - service is ready
                    ready.push((bus_name.clone(), service_name.clone()));
                }
                Err(_) => {
                    // Name not owned yet
                }
            }
        }

        // Mark ready services as running
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

                // Set watchdog deadline for Type=dbus services
                if let Some(wd) = self
                    .units
                    .get(&service_name)
                    .and_then(|u| u.as_service())
                    .and_then(|s| s.service.watchdog_sec)
                {
                    self.watchdog_deadlines
                        .insert(service_name.clone(), std::time::Instant::now() + wd);
                }
            }
        }
    }

    /// Check for watchdog timeouts and restart services that missed their deadline
    pub async fn process_watchdog(&mut self) {
        let now = std::time::Instant::now();
        let mut timed_out = Vec::new();

        for (name, deadline) in &self.watchdog_deadlines {
            if now > *deadline {
                timed_out.push(name.clone());
            }
        }

        for name in timed_out {
            self.watchdog_deadlines.remove(&name);
            log::warn!("{} watchdog timeout - restarting", name);

            // Kill the service and let restart policy handle it
            if let Some(mut child) = self.processes.remove(&name) {
                if let Some(pid) = child.id() {
                    // Send SIGABRT (standard watchdog signal) then SIGKILL
                    unsafe {
                        libc::kill(pid as i32, libc::SIGABRT);
                    }
                    // Give it a moment, then force kill
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let _ = child.kill().await;
                }
            }

            // Update state to failed with watchdog reason
            if let Some(state) = self.states.get_mut(&name) {
                state.set_failed("Watchdog timeout".to_string());
            }

            // Schedule restart based on policy
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
