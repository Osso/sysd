impl Manager {
    async fn resolve_start_unit_name(&mut self, name: &str) -> Result<String, ManagerError> {
        if self.units.contains_key(name) {
            return Ok(name.to_string());
        }
        self.load(name).await
    }

    async fn start_non_service_unit(
        &mut self,
        actual_name: &str,
        unit: &Unit,
    ) -> Result<bool, ManagerError> {
        if unit.is_target() {
            return Err(ManagerError::IsTarget(actual_name.to_string()));
        }
        if let Some(slice) = unit.as_slice().cloned() {
            self.start_slice(actual_name, &slice).await?;
            return Ok(true);
        }
        if let Some(socket) = unit.as_socket().cloned() {
            self.start_socket(actual_name, &socket).await?;
            return Ok(true);
        }
        if let Some(timer) = unit.as_timer().cloned() {
            self.start_timer(actual_name, &timer).await?;
            return Ok(true);
        }
        if let Some(path_unit) = unit.as_path().cloned() {
            self.start_path(actual_name, &path_unit).await?;
            return Ok(true);
        }
        if let Some(reason) = self.check_conditions(unit) {
            log::info!("{}: condition failed: {}", actual_name, reason);
            return Err(ManagerError::ConditionFailed(actual_name.to_string(), reason));
        }
        if let Some(mount) = unit.as_mount().cloned() {
            self.start_mount(actual_name, &mount).await?;
            return Ok(true);
        }
        Ok(false)
    }

    async fn start_service_unit(
        &mut self,
        actual_name: &str,
        service: Service,
    ) -> Result<(), ManagerError> {
        self.mark_service_starting(actual_name)?;
        if service.service.service_type == ServiceType::Idle {
            self.wait_for_idle_queue(actual_name).await;
        }

        let (socket_fds, socket_fd_names) = self.prepare_socket_fds(&service, actual_name);
        let (dynamic_uid, dynamic_gid) = self.allocate_dynamic_user(actual_name, &service)?;
        let options = self.build_spawn_options(
            &service,
            actual_name,
            socket_fds,
            socket_fd_names,
            dynamic_uid,
            dynamic_gid,
        );

        if service.service.service_type == ServiceType::Oneshot {
            return self.start_oneshot_service(actual_name, &service, options);
        }

        let child = process::spawn_service_via_executor(&service, &options, &self.executor_path, 0)?;
        let pid = self.log_spawned_pid(actual_name, &child);
        let limits = service_cgroup_limits(&service);
        let slice = service.service.slice.as_deref().map(str::to_string);
        self.setup_cgroup_for_service(
            actual_name,
            pid,
            &limits,
            slice.as_deref(),
            service.service.delegate,
        );

        self.processes.insert(actual_name.to_string(), child);
        self.pid_to_service.insert(pid, actual_name.to_string());
        self.configure_post_spawn_state(actual_name, pid, &service);
        Ok(())
    }

    fn mark_service_starting(&mut self, actual_name: &str) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(actual_name)
            .ok_or_else(|| ManagerError::NotFound(actual_name.to_string()))?;
        if state.is_active() {
            return Err(ManagerError::AlreadyActive(actual_name.to_string()));
        }
        state.set_starting();
        self.active_jobs += 1;
        Ok(())
    }

    fn log_oneshot_start(&self, actual_name: &str, service: &Service) -> usize {
        let num_commands = service.service.exec_start.len();
        log::info!(
            "Starting oneshot {} ({} command{})",
            actual_name,
            num_commands,
            if num_commands == 1 { "" } else { "s" }
        );
        num_commands
    }

    fn log_spawned_pid(&self, actual_name: &str, child: &Child) -> u32 {
        log::debug!("{}: spawn returned, getting PID", actual_name);
        let pid = child.id().unwrap_or(0);
        log::debug!("{}: PID is {}", actual_name, pid);
        pid
    }

    fn spawn_initial_oneshot_completion_task(
        &self,
        child: Child,
        service_name: String,
        total_cmds: usize,
        remain_after_exit: bool,
    ) {
        let tx = self.oneshot_completion_tx.clone();
        tokio::spawn(async move {
            let result = child.wait_with_output().await;
            let (exit_code, error) = oneshot_completion_result(result);
            let _ = tx
                .send(OneshotCompletion {
                    service_name,
                    cmd_idx: 0,
                    total_cmds,
                    exit_code,
                    error,
                    remain_after_exit,
                })
                .await;
        });
    }

    fn mark_notify_start(&mut self, actual_name: &str, pid: u32) {
        self.waiting_ready.insert(pid, actual_name.to_string());
        log::info!("Started {} (PID {}), waiting for READY", actual_name, pid);
    }

    fn mark_dbus_start(&mut self, actual_name: &str, pid: u32, service: &Service) {
        if let Some(bus_name) = service.service.bus_name.as_ref() {
            self.waiting_bus_name
                .insert(bus_name.clone(), actual_name.to_string());
            log::info!(
                "Started {} (PID {}), waiting for D-Bus name {}",
                actual_name,
                pid,
                bus_name
            );
            return;
        }
        log::warn!(
            "{} is Type=dbus but has no BusName=, treating as simple",
            actual_name
        );
        self.mark_running_start(actual_name, pid, service);
    }

    fn mark_forking_start(&mut self, actual_name: &str, pid: u32, service: &Service) {
        log::info!("Started {} (PID {}), waiting for fork", actual_name, pid);
        let Some(pid_file) = service.service.pid_file.as_ref() else {
            return;
        };
        log::debug!("{} will read PID from {}", actual_name, pid_file.display());
        self.pid_files
            .insert(actual_name.to_string(), pid_file.clone());
    }

    fn mark_running_start(&mut self, actual_name: &str, pid: u32, service: &Service) {
        if let Some(state) = self.states.get_mut(actual_name) {
            state.set_running(pid);
        }
        self.active_jobs = self.active_jobs.saturating_sub(1);
        if let Some(wd) = service.service.watchdog_sec {
            self.watchdog_deadlines
                .insert(actual_name.to_string(), std::time::Instant::now() + wd);
        }
        log::info!("Started {} (PID {})", actual_name, pid);
    }

    fn log_missing_cgroup_support(&self, name: &str, has_resource_limits: bool) {
        if has_resource_limits {
            log::error!(
                "Service {} requests resource limits but cgroups unavailable - limits NOT enforced",
                name
            );
        }
    }

    fn log_cgroup_setup_error(&self, name: &str, has_resource_limits: bool, err: std::io::Error) {
        if has_resource_limits {
            log::error!(
                "Failed to set up cgroup for {} (resource limits NOT enforced): {}",
                name,
                err
            );
            return;
        }
        log::warn!("Failed to set up cgroup for {}: {}", name, err);
    }

    fn enable_service_delegation(
        &self,
        cgroup_mgr: &CgroupManager,
        name: &str,
        cgroup_path: &PathBuf,
    ) {
        if let Err(e) = cgroup_mgr.enable_delegation(cgroup_path) {
            log::warn!("Failed to enable cgroup delegation for {}: {}", name, e);
        }
    }

    async fn wait_for_idle_queue(&mut self, name: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.active_jobs > 1 && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if self.active_jobs > 1 {
            log::debug!("{}: idle timeout, proceeding anyway", name);
        }
    }

    fn prepare_socket_fds(
        &self,
        service: &Service,
        actual_name: &str,
    ) -> (Vec<RawFd>, Vec<String>) {
        let socket_fds = self.get_socket_fds(&service.name);
        let socket_fd_names = self.get_socket_fd_names(&service.name);
        if !socket_fds.is_empty() {
            log::info!(
                "{}: passing socket FDs {:?} names {:?}",
                actual_name,
                socket_fds,
                socket_fd_names
            );
        } else if !service.service.sockets.is_empty() {
            log::warn!(
                "{}: has Sockets={:?} but got NO socket FDs! socket_fds keys: {:?}",
                actual_name,
                service.service.sockets,
                self.socket_fds.keys().collect::<Vec<_>>()
            );
        }
        (socket_fds, socket_fd_names)
    }

    fn allocate_dynamic_user(
        &mut self,
        name: &str,
        service: &Service,
    ) -> Result<(Option<u32>, Option<u32>), ManagerError> {
        if service.service.dynamic_user {
            match self.dynamic_user_manager.allocate(name) {
                Ok((uid, gid)) => {
                    self.dynamic_uids.insert(name.to_string(), uid);
                    log::info!("Allocated dynamic UID/GID {} for {}", uid, name);
                    Ok((Some(uid), Some(gid)))
                }
                Err(e) => {
                    log::error!("Failed to allocate dynamic user for {}: {}", name, e);
                    Err(ManagerError::StartFailed(e.to_string()))
                }
            }
        } else {
            Ok((None, None))
        }
    }

    fn build_spawn_options(
        &self,
        service: &Service,
        actual_name: &str,
        socket_fds: Vec<RawFd>,
        socket_fd_names: Vec<String>,
        dynamic_uid: Option<u32>,
        dynamic_gid: Option<u32>,
    ) -> SpawnOptions {
        let is_notify = service.service.service_type == ServiceType::Notify;
        let watchdog_usec = service.service.watchdog_sec.map(|d| d.as_micros() as u64);
        let stored_fds: Vec<RawFd> = self
            .fd_store
            .get(actual_name)
            .map(|fds| fds.iter().map(|(_, fd)| *fd).collect())
            .unwrap_or_default();
        let options = SpawnOptions {
            notify_socket: if is_notify || watchdog_usec.is_some() {
                self.notify_socket_path()
                    .map(|p| p.to_string_lossy().to_string())
            } else {
                None
            },
            watchdog_usec,
            socket_fds,
            socket_fd_names,
            dynamic_uid,
            dynamic_gid,
            stored_fds,
            user_environment: self.user_environment.clone(),
        };
        if is_notify {
            log::debug!(
                "{}: Type=notify, NOTIFY_SOCKET={:?}",
                actual_name,
                options.notify_socket
            );
        }
        options
    }

    fn start_oneshot_service(
        &mut self,
        actual_name: &str,
        service: &Service,
        options: SpawnOptions,
    ) -> Result<(), ManagerError> {
        let num_commands = self.log_oneshot_start(actual_name, service);
        let child = process::spawn_service_via_executor(service, &options, &self.executor_path, 0)?;
        let pid = self.log_spawned_pid(actual_name, &child);
        let limits = service_cgroup_limits(service);
        let slice = service.service.slice.as_deref().map(str::to_string);
        let delegate = service.service.delegate;
        self.setup_cgroup_for_service(actual_name, pid, &limits, slice.as_deref(), delegate);
        log::info!("Started {} (PID {})", actual_name, pid);

        self.spawn_initial_oneshot_completion_task(
            child,
            actual_name.to_string(),
            num_commands,
            service.service.remain_after_exit,
        );
        Ok(())
    }

    fn configure_post_spawn_state(&mut self, actual_name: &str, pid: u32, service: &Service) {
        match service.service.service_type {
            ServiceType::Notify => self.mark_notify_start(actual_name, pid),
            ServiceType::Dbus => self.mark_dbus_start(actual_name, pid, service),
            ServiceType::Forking => self.mark_forking_start(actual_name, pid, service),
            _ => self.mark_running_start(actual_name, pid, service),
        };
    }

    /// Start a single unit (internal, assumes already loaded)
    async fn start_single(&mut self, name: &str) -> Result<(), ManagerError> {
        self.log_start_single_request(name);
        let actual_name = self.resolve_start_unit_name(name).await?;
        let unit = self
            .units
            .get(&actual_name)
            .cloned()
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;
        if self.start_non_service_unit(&actual_name, &unit).await? {
            return Ok(());
        }
        let service = unit
            .as_service()
            .cloned()
            .ok_or_else(|| ManagerError::NotFound(actual_name.to_string()))?;
        self.start_service_unit(&actual_name, service).await
    }

    /// Set up cgroup for a spawned service process
    fn setup_cgroup_for_service(
        &mut self,
        name: &str,
        pid: u32,
        limits: &CgroupLimits,
        slice: Option<&str>,
        delegate: bool,
    ) {
        let has_resource_limits = limits.memory_max.is_some()
            || limits.cpu_quota.is_some()
            || limits.tasks_max.is_some();

        let Some(cgroup_mgr) = self.cgroup_manager.as_ref() else {
            self.log_missing_cgroup_support(name, has_resource_limits);
            return;
        };

        let cgroup_path = match cgroup_mgr.setup_service_cgroup(name, pid, limits, slice) {
            Ok(path) => path,
            Err(e) => {
                self.log_cgroup_setup_error(name, has_resource_limits, e);
                return;
            }
        };

        log::debug!("Created cgroup {} for {}", cgroup_path.display(), name);
        if delegate {
            self.enable_service_delegation(cgroup_mgr, name, &cgroup_path);
        }
        self.cgroup_paths.insert(name.to_string(), cgroup_path);
    }

    async fn send_signals_to_child(
        child: &mut Child,
        kill_mode: &KillMode,
        send_sighup: bool,
        name: &str,
    ) {
        let Some(pid) = child.id() else { return };
        log::info!("Stopping {} (PID {}, KillMode={:?})", name, pid, kill_mode);
        if send_sighup {
            log::debug!("Sending SIGHUP to {} (PID {})", name, pid);
            unsafe { libc::kill(pid as i32, libc::SIGHUP) };
        }
        match kill_mode {
            KillMode::None => {}
            KillMode::Process => unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            },
            KillMode::Mixed | KillMode::ControlGroup => unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            },
        }
    }

    async fn wait_for_child_exit(&mut self, name: &str, mut child: Child) {
        let timeout_sec = self
            .units
            .get(name)
            .and_then(|u| u.as_service())
            .and_then(|s| s.service.timeout_stop_sec)
            .unwrap_or(std::time::Duration::from_secs(10));

        match tokio::time::timeout(timeout_sec, child.wait()).await {
            Ok(Ok(status)) => {
                let code = status.code().unwrap_or(-1);
                if let Some(state) = self.states.get_mut(name) {
                    state.set_stopped(code);
                }
                log::info!("Stopped {} (exit code {})", name, code);
            }
            Ok(Err(e)) => {
                if let Some(state) = self.states.get_mut(name) {
                    state.set_failed(e.to_string());
                }
            }
            Err(_) => {
                log::warn!("Timeout stopping {}, sending SIGKILL", name);
                if let Some(pid) = child.id() {
                    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
                }
                let _ = child.wait().await;
                if let Some(state) = self.states.get_mut(name) {
                    state.set_stopped(-9);
                }
            }
        }
    }

    fn cleanup_runtime_dirs(&self, name: &str) {
        if let Some(service) = self.units.get(name) {
            if let crate::units::Unit::Service(svc) = service {
                use crate::units::RuntimeDirectoryPreserve;
                match svc.service.runtime_directory_preserve {
                    RuntimeDirectoryPreserve::No | RuntimeDirectoryPreserve::Restart => {
                        cleanup_runtime_directories(&svc.service, name);
                    }
                    RuntimeDirectoryPreserve::Yes => {}
                }
            }
        }
    }

    async fn run_stop_post_commands(&self, name: &str) {
        let Some(svc) = self.units.get(name).and_then(|unit| unit.as_service()) else {
            return;
        };
        for cmd_line in &svc.service.exec_stop_post {
            log::debug!("Running ExecStopPost for {}: {}", name, cmd_line);
            if let Err(e) = run_simple_command(cmd_line).await {
                log::warn!("ExecStopPost failed for {}: {}", name, e);
            }
        }
    }

}
