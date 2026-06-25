impl Manager {
    /// Take the oneshot completion receiver (for use in background task)
    pub fn take_oneshot_completion_rx(
        &mut self,
    ) -> Option<tokio::sync::mpsc::Receiver<OneshotCompletion>> {
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
    pub async fn handle_oneshot_completion(&mut self, completion: OneshotCompletion) {
        let service_name = &completion.service_name;
        log::debug!(
            "Oneshot {} command {} completed (exit={:?}, error={:?})",
            service_name,
            completion.cmd_idx,
            completion.exit_code,
            completion.error
        );

        if let Some(ref error) = completion.error {
            self.handle_oneshot_failure(service_name, error);
            return;
        }

        let next_idx = completion.cmd_idx + 1;
        if next_idx < completion.total_cmds {
            self.start_next_oneshot_command(service_name, next_idx, &completion)
                .await;
            return;
        }

        self.finish_oneshot_success(service_name, completion.remain_after_exit);
    }

    fn handle_oneshot_failure(&mut self, service_name: &str, error: &str) {
        self.fail_oneshot(service_name, error);
        log::warn!("Oneshot {} failed: {}", service_name, error);
    }

    async fn start_next_oneshot_command(
        &mut self,
        service_name: &str,
        next_idx: usize,
        completion: &OneshotCompletion,
    ) {
        log::debug!(
            "Oneshot {} starting command {} of {}",
            service_name,
            next_idx + 1,
            completion.total_cmds
        );
        self.pending_oneshot_cmds.insert(
            service_name.to_string(),
            (next_idx, completion.total_cmds, completion.remain_after_exit),
        );
        if let Err(e) = self.start_oneshot_command(service_name, next_idx).await {
            let msg = format!("Command {} failed: {}", next_idx, e);
            self.fail_oneshot(service_name, &msg);
            log::warn!(
                "Oneshot {} command {} failed to start: {}",
                service_name,
                next_idx,
                e
            );
        }
    }

    fn finish_oneshot_success(&mut self, service_name: &str, remain_after_exit: bool) {
        self.cleanup_oneshot_cgroup(service_name);
        self.active_jobs = self.active_jobs.saturating_sub(1);
        if let Some(state) = self.states.get_mut(service_name) {
            if remain_after_exit {
                state.set_exited();
            } else {
                state.set_inactive();
            }
        }
        self.pending_oneshot_cmds.remove(service_name);
        log::info!("Oneshot {} completed successfully (exit 0)", service_name);
    }

    /// Start a specific command of a oneshot service (used for multi-command oneshots)
    async fn start_oneshot_command(
        &mut self,
        service_name: &str,
        cmd_idx: usize,
    ) -> Result<(), ManagerError> {
        let service = self.get_oneshot_service(service_name)?;
        let total_cmds = service.service.exec_start.len();
        let remain_after_exit = service.service.remain_after_exit;
        let child = self.spawn_oneshot_child(&service, cmd_idx)?;
        let pid = child.id().unwrap_or_default();

        log::info!(
            "Oneshot {} command {} started (PID {})",
            service_name,
            cmd_idx,
            pid
        );

        self.add_oneshot_pid_to_cgroup(service_name, pid);
        self.spawn_oneshot_completion_task(service_name, cmd_idx, total_cmds, remain_after_exit, child);

        Ok(())
    }

    fn get_oneshot_service(&self, service_name: &str) -> Result<crate::units::Service, ManagerError> {
        self.units
            .get(service_name)
            .and_then(|u| u.as_service())
            .cloned()
            .ok_or_else(|| ManagerError::NotFound(service_name.to_string()))
    }

    fn spawn_oneshot_child(
        &self,
        service: &crate::units::Service,
        cmd_idx: usize,
    ) -> Result<tokio::process::Child, ManagerError> {
        let options = SpawnOptions::default();
        process::spawn_service_via_executor(service, &options, &self.executor_path, cmd_idx)
            .map_err(ManagerError::from)
    }

    fn add_oneshot_pid_to_cgroup(&self, service_name: &str, pid: u32) {
        let Some(cgroup_path) = self.cgroup_paths.get(service_name) else {
            return;
        };
        let Some(cgroup_mgr) = &self.cgroup_manager else {
            return;
        };
        if let Err(e) = cgroup_mgr.add_pid(cgroup_path, pid) {
            log::warn!(
                "Failed to add PID {} to cgroup for {}: {}",
                pid,
                service_name,
                e
            );
        }
    }

    fn spawn_oneshot_completion_task(
        &self,
        service_name: &str,
        cmd_idx: usize,
        total_cmds: usize,
        remain_after_exit: bool,
        child: tokio::process::Child,
    ) {
        let service_name = service_name.to_string();
        let tx = self.oneshot_completion_tx.clone();
        tokio::spawn(async move {
            let (exit_code, error) = read_oneshot_completion_result(child).await;
            let _ = tx
                .send(OneshotCompletion {
                    service_name,
                    cmd_idx,
                    total_cmds,
                    exit_code,
                    error,
                    remain_after_exit,
                })
                .await;
        });
    }

    /// Check if any Type=dbus services have acquired their bus name
    pub async fn process_dbus_ready(&mut self) {
        if self.waiting_bus_name.is_empty() {
            return;
        }

        let Some(conn) = self.open_system_bus().await else {
            return;
        };
        let ready = self.collect_ready_dbus_names(&conn).await;
        for (bus_name, service_name) in ready {
            self.mark_dbus_service_ready(&bus_name, &service_name);
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
            self.handle_watchdog_timeout(&name).await;
        }
    }

    async fn open_system_bus(&self) -> Option<zbus::Connection> {
        match zbus::Connection::system().await {
            Ok(conn) => Some(conn),
            Err(e) => {
                log::debug!("Cannot check D-Bus names (no connection): {}", e);
                None
            }
        }
    }

    async fn collect_ready_dbus_names(
        &self,
        conn: &zbus::Connection,
    ) -> Vec<(String, String)> {
        let mut ready = Vec::new();
        for (bus_name, service_name) in &self.waiting_bus_name {
            if dbus_name_has_owner(conn, bus_name).await {
                ready.push((bus_name.clone(), service_name.clone()));
            }
        }
        ready
    }

    fn mark_dbus_service_ready(&mut self, bus_name: &str, service_name: &str) {
        self.waiting_bus_name.remove(bus_name);
        if let Some(state) = self.states.get_mut(service_name) {
            let pid = self
                .processes
                .get(service_name)
                .and_then(|c| c.id())
                .unwrap_or_default();
            state.set_running(pid);
            self.active_jobs = self.active_jobs.saturating_sub(1);
            log::info!("{} acquired D-Bus name {}", service_name, bus_name);
        }
        self.arm_watchdog(service_name);
    }

    async fn handle_watchdog_timeout(&mut self, service_name: &str) {
        self.watchdog_deadlines.remove(service_name);
        log::warn!("{} watchdog timeout - restarting", service_name);
        self.abort_watchdog_process(service_name).await;
        self.mark_watchdog_failure(service_name);
        self.schedule_watchdog_restart_if_needed(service_name);
    }

    async fn abort_watchdog_process(&mut self, service_name: &str) {
        let Some(mut child) = self.processes.remove(service_name) else {
            return;
        };
        let Some(pid) = child.id() else {
            return;
        };
        unsafe {
            libc::kill(pid as i32, libc::SIGABRT);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let _ = child.kill().await;
    }

    fn mark_watchdog_failure(&mut self, service_name: &str) {
        if let Some(state) = self.states.get_mut(service_name) {
            state.set_failed("Watchdog timeout".to_string());
        }
    }

    fn schedule_watchdog_restart_if_needed(&mut self, service_name: &str) {
        let Some(restart_sec) = self.watchdog_restart_delay(service_name) else {
            return;
        };
        if let Some(state) = self.states.get_mut(service_name) {
            state.set_auto_restart(restart_sec);
            log::info!(
                "{} scheduling watchdog restart in {:?}",
                service_name,
                restart_sec
            );
        }
    }

    fn watchdog_restart_delay(&self, service_name: &str) -> Option<std::time::Duration> {
        let service = self.units.get(service_name).and_then(|u| u.as_service())?;
        if matches!(
            service.service.restart,
            RestartPolicy::Always | RestartPolicy::OnFailure
        ) {
            return Some(service.service.restart_sec);
        }
        None
    }
}
struct RestartDecisionInput {
    restart_policy: RestartPolicy,
    restart_sec: std::time::Duration,
    remain_after_exit: bool,
    is_oneshot: bool,
    is_forking: bool,
    start_limit_burst: Option<u32>,
    start_limit_interval_sec: Option<std::time::Duration>,
    restart_prevent_exit_status: Vec<i32>,
}

impl Default for RestartDecisionInput {
    fn default() -> Self {
        Self {
            restart_policy: RestartPolicy::No,
            restart_sec: std::time::Duration::from_millis(100),
            remain_after_exit: false,
            is_oneshot: false,
            is_forking: false,
            start_limit_burst: None,
            start_limit_interval_sec: None,
            restart_prevent_exit_status: Vec::new(),
        }
    }
}

fn close_fds(fds: &[i32]) {
    for fd in fds {
        unsafe {
            libc::close(*fd);
        }
    }
}

fn store_fd_entries(
    store: &mut Vec<(String, i32)>,
    service_name: &str,
    fd_name: &str,
    fds: &[i32],
    max_fds: usize,
) {
    for fd in fds {
        if store.len() >= max_fds {
            log::warn!(
                "{}: FD store full (max {}), closing extra FD",
                service_name,
                max_fds
            );
            unsafe {
                libc::close(*fd);
            }
            continue;
        }
        log::debug!("{}: Storing FD {} as '{}'", service_name, fd, fd_name);
        store.push((fd_name.to_string(), *fd));
    }
}

async fn read_oneshot_completion_result(
    child: tokio::process::Child,
) -> (Option<i32>, Option<String>) {
    match child.wait_with_output().await {
        Ok(output) => {
            let code = output.status.code().unwrap_or(-1);
            if code == 0 {
                (Some(0), None)
            } else {
                (Some(code), Some(format!("exit code {}", code)))
            }
        }
        Err(e) => (None, Some(e.to_string())),
    }
}

async fn dbus_name_has_owner(conn: &zbus::Connection, bus_name: &str) -> bool {
    conn.call_method(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        Some("org.freedesktop.DBus"),
        "GetNameOwner",
        &(bus_name,),
    )
    .await
    .is_ok()
}
