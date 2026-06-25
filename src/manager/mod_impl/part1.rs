impl Manager {
    /// Create a new service manager for system mode
    pub fn new() -> Self {
        Self::with_mode(false)
    }

    /// Create a new service manager for user mode
    pub fn new_user() -> Self {
        Self::with_mode(true)
    }

    /// Create a service manager with explicit mode
    fn with_mode(user_mode: bool) -> Self {
        let cgroup_manager = Self::init_cgroup_manager(user_mode);
        let (socket_activation_tx, socket_activation_rx) = mpsc::channel(32);
        let (timer_tx, timer_rx) = mpsc::channel(32);
        let (path_tx, path_rx) = mpsc::channel(32);
        let (oneshot_completion_tx, oneshot_completion_rx) = mpsc::channel(32);
        let unit_paths = Self::unit_paths_for_mode(user_mode);
        let scope_manager = ScopeManager::new(cgroup_manager.clone());
        let executor_path = Self::resolve_executor_path();

        Self {
            units: HashMap::new(), states: HashMap::new(), processes: HashMap::new(),
            unit_paths,
            notify_listener: None, notify_rx: None, waiting_ready: HashMap::new(),
            cgroup_manager, cgroup_paths: HashMap::new(), pid_files: HashMap::new(),
            active_jobs: 0,
            waiting_bus_name: HashMap::new(), watchdog_deadlines: HashMap::new(),
            socket_fds: HashMap::new(), socket_activation_tx, socket_activation_rx: Some(socket_activation_rx),
            timer_tx, timer_rx: Some(timer_rx), path_tx, path_rx: Some(path_rx),
            boot_time: std::time::Instant::now(),
            scope_manager, dynamic_user_manager: dynamic_user::DynamicUserManager::new(),
            dynamic_uids: HashMap::new(), fd_store: HashMap::new(),
            executor_path,
            pid_to_service: HashMap::new(), oneshot_completion_tx,
            oneshot_completion_rx: Some(oneshot_completion_rx),
            pending_oneshot_cmds: HashMap::new(), user_environment: HashMap::new(),
            user_mode,
        }
    }

    fn init_cgroup_manager(user_mode: bool) -> Option<CgroupManager> {
        if user_mode {
            return None;
        }
        match CgroupManager::new() {
            Ok(mgr) => {
                log::debug!("Cgroup manager initialized");
                Some(mgr)
            }
            Err(e) => {
                log::debug!(
                    "Cgroup manager unavailable: {} (running without cgroups)",
                    e
                );
                None
            }
        }
    }

    fn unit_paths_for_mode(user_mode: bool) -> Vec<PathBuf> {
        if user_mode {
            return Self::user_unit_paths();
        }
        vec![
            PathBuf::from("/etc/systemd/system"),
            PathBuf::from("/usr/lib/systemd/system"),
        ]
    }

    fn resolve_executor_path() -> String {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("sysd-executor")))
            .filter(|p| p.exists())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "sysd-executor".to_string())
    }

    /// Get unit search paths for user mode
    fn user_unit_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut seen = HashSet::new();

        // User-specific config directory (highest priority)
        if let Some(config_dir) = dirs::config_dir() {
            push_unique_path(&mut paths, &mut seen, config_dir.join("systemd/user"));
        }
        // Also check XDG_CONFIG_HOME or fallback to ~/.config
        if let Ok(home) = std::env::var("HOME") {
            let user_config = PathBuf::from(&home).join(".config/systemd/user");
            push_unique_path(&mut paths, &mut seen, user_config);
        }

        // System-wide user unit directories
        push_unique_path(&mut paths, &mut seen, PathBuf::from("/etc/systemd/user"));
        push_unique_path(
            &mut paths,
            &mut seen,
            PathBuf::from("/usr/lib/systemd/user"),
        );

        // XDG data directories for user units
        if let Some(data_dir) = dirs::data_dir() {
            push_unique_path(&mut paths, &mut seen, data_dir.join("systemd/user"));
        }

        paths
    }

    /// Check if user has lingering enabled
    pub fn is_lingering(username: &str) -> bool {
        std::path::Path::new(&format!("/var/lib/systemd/linger/{}", username)).exists()
    }

    /// Get the current user's runtime directory
    pub fn user_runtime_dir() -> Option<PathBuf> {
        std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                let uid = unsafe { libc::getuid() };
                let path = PathBuf::from(format!("/run/user/{}", uid));
                if path.exists() {
                    Some(path)
                } else {
                    None
                }
            })
    }

    /// Ensure XDG_RUNTIME_DIR exists and has correct permissions
    pub fn ensure_runtime_dir() -> std::io::Result<PathBuf> {
        let uid = unsafe { libc::getuid() };
        let runtime_dir = PathBuf::from(format!("/run/user/{}", uid));

        if !runtime_dir.exists() {
            std::fs::create_dir_all(&runtime_dir)?;
            // Set permissions to 0700 (owner only)
            std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))?;
        }

        // Set environment variable if not already set
        if std::env::var("XDG_RUNTIME_DIR").is_err() {
            std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
        }

        Ok(runtime_dir)
    }

    /// Check if cgroup management is available
    pub fn cgroups_available(&self) -> bool {
        self.cgroup_manager.is_some()
    }

    /// Check if running in user mode
    pub fn is_user_mode(&self) -> bool {
        self.user_mode
    }

    /// Get the directory for enable/disable symlinks
    ///
    /// In system mode: /etc/systemd/system
    /// In user mode: ~/.config/systemd/user (first user path)
    pub fn enable_dir(&self) -> PathBuf {
        if self.user_mode {
            // Use first user path (highest priority, typically ~/.config/systemd/user)
            self.unit_paths
                .first()
                .cloned()
                .unwrap_or_else(|| PathBuf::from("/etc/systemd/user"))
        } else {
            PathBuf::from("/etc/systemd/system")
        }
    }

    /// Initialize the notify socket listener
    pub fn init_notify_socket(&mut self) -> std::io::Result<()> {
        let socket_path = self.notify_socket_path_for_mode();
        let (listener, rx) = AsyncNotifyListener::new(std::path::Path::new(&socket_path))?;
        self.notify_listener = Some(listener);
        self.notify_rx = Some(rx);
        log::info!("Notify socket listening at {}", socket_path);
        Ok(())
    }

    /// Get the notify socket path based on mode (system vs user)
    fn notify_socket_path_for_mode(&self) -> String {
        if self.user_mode {
            // User mode: /run/user/<uid>/sysd/notify
            let uid = nix::unistd::getuid().as_raw();
            format!("/run/user/{}/sysd/notify", uid)
        } else {
            // System mode: /run/sysd/notify
            NOTIFY_SOCKET_PATH.to_string()
        }
    }

    /// Get the notify socket path (if initialized)
    pub fn notify_socket_path(&self) -> Option<&std::path::Path> {
        self.notify_listener.as_ref().map(|l| l.socket_path())
    }

    /// Load a unit (service or target) by name
    /// Load a unit by name, returning the canonical name it was stored under
    /// (may differ from input if the unit file is a symlink)
    pub async fn load(&mut self, name: &str) -> Result<String, ManagerError> {
        let name = match self.resolve_load_name(name).await? {
            LoadNameResolution::Continue(name) => name,
            LoadNameResolution::AlreadyLoaded(name) => return Ok(name),
        };

        if self.units.contains_key(&name) {
            return Ok(name);
        }

        let path = self.find_unit(&name)?;
        let canonical_name = self.resolve_canonical_unit_name(&name, &path)?;

        if self.units.contains_key(&canonical_name) {
            return Ok(canonical_name);
        }

        let mut unit = self.parse_unit_file(&path).await?;
        self.apply_canonical_name(&mut unit, &canonical_name);
        self.states.insert(canonical_name.clone(), ServiceState::new());
        self.units.insert(canonical_name.clone(), unit);

        Ok(canonical_name)
    }

    async fn resolve_load_name(&mut self, name: &str) -> Result<LoadNameResolution, ManagerError> {
        let name = self.normalize_name(name);
        if !units::is_bare_template(&name) {
            return Ok(LoadNameResolution::Continue(name));
        }

        let path = self.find_unit(&name)?;
        let unit = self.parse_unit_file(&path).await?;
        let default_instance = default_instance_for_unit(&unit);
        if let Some(instance) = default_instance {
            if let Some(instantiated) = units::instantiate_template(&name, &instance) {
                log::debug!(
                    "Template {} has DefaultInstance={}, loading {}",
                    name,
                    instance,
                    instantiated
                );
                return Ok(LoadNameResolution::Continue(instantiated));
            }
        }

        let stored_name = name.clone();
        self.states.insert(name.clone(), ServiceState::new());
        self.units.insert(name, unit);
        Ok(LoadNameResolution::AlreadyLoaded(stored_name))
    }

    async fn parse_unit_file(&self, path: &std::path::Path) -> Result<Unit, ManagerError> {
        units::load_unit(path)
            .await
            .map_err(|e| ManagerError::Parse(e.to_string()))
    }

    fn resolve_canonical_unit_name(
        &self,
        requested_name: &str,
        path: &std::path::Path,
    ) -> Result<String, ManagerError> {
        if !path.is_symlink() {
            return Ok(requested_name.to_string());
        }

        let Ok(target) = std::fs::read_link(path) else {
            return Ok(requested_name.to_string());
        };
        if target.as_os_str() == "/dev/null" {
            log::debug!("{} is masked, skipping", requested_name);
            return Err(ManagerError::Masked(requested_name.to_string()));
        }

        let target_name = target
            .file_name()
            .and_then(|f| f.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| requested_name.to_string());

        let Some(instance) = units::extract_instance(requested_name) else {
            return Ok(target_name);
        };
        if !units::is_bare_template(&target_name) {
            return Ok(target_name);
        }
        Ok(units::instantiate_template(&target_name, &instance).unwrap_or(target_name))
    }

    fn apply_canonical_name(&self, unit: &mut Unit, canonical_name: &str) {
        if unit.name() == canonical_name {
            return;
        }
        log::debug!(
            "Updating unit name from {} to {} (template instantiation)",
            unit.name(),
            canonical_name
        );
        unit.set_name(canonical_name.to_string());
    }

    /// Load a unit from a specific path
    pub async fn load_from_path(&mut self, path: &std::path::Path) -> Result<(), ManagerError> {
        let unit = units::load_unit(path)
            .await
            .map_err(|e| ManagerError::Parse(e.to_string()))?;

        let name = unit.name().to_string();
        self.states.insert(name.clone(), ServiceState::new());
        self.units.insert(name, unit);

        Ok(())
    }

    /// Find a unit file in search paths
    fn find_unit(&self, name: &str) -> Result<PathBuf, ManagerError> {
        if let Some(path) = self.search_unit_paths(name) {
            return Ok(path);
        }

        if let Some(template_name) = units::get_template_name(name) {
            if let Some(path) = self.search_unit_paths(&template_name) {
                return Ok(path);
            }
        }

        Err(ManagerError::NotFound(name.to_string()))
    }

    fn search_unit_paths(&self, name: &str) -> Option<PathBuf> {
        for base in &self.unit_paths {
            let path = base.join(name);
            if path.exists() {
                return Some(path);
            }
            if path.is_symlink() {
                if let Ok(target) = std::fs::read_link(&path) {
                    if target.exists() {
                        return Some(path);
                    }
                }
            }
        }
        None
    }

    /// Start a single service (no dependency resolution)
    pub async fn start(&mut self, name: &str) -> Result<(), ManagerError> {
        let name = self.normalize_name(name);
        match self.start_single(&name).await {
            Ok(()) => Ok(()),
            Err(ManagerError::IsTarget(_)) => {
                // Targets are synchronization points - just mark as active
                if let Some(state) = self.states.get_mut(&name) {
                    state.set_running(0);
                }
                log::debug!("Target {} reached", name);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Start a unit with all its dependencies
    pub async fn start_with_deps(&mut self, name: &str) -> Result<Vec<String>, ManagerError> {
        let name = self.normalize_name(name);
        let order = self.resolve_start_order(&name).await?;
        log::info!("Start order for {}: {:?}", name, order);

        let mut started = Vec::new();
        for unit_name in &order {
            self.start_dependency_unit(&name, unit_name, &mut started)
                .await?;
        }

        Ok(started)
    }

    /// Resolve start order for a unit and its dependencies
    async fn resolve_start_order(&mut self, name: &str) -> Result<Vec<String>, ManagerError> {
        self.ensure_unit_loaded(name).await?;
        let (loaded, aliases) = self.collect_start_dependencies(name).await;
        let graph = self.build_start_graph(&loaded, &aliases);
        graph
            .start_order_for(name)
            .map_err(|e| ManagerError::Cycle(e.nodes))
    }

    async fn start_dependency_unit(
        &mut self,
        root_name: &str,
        unit_name: &str,
        started: &mut Vec<String>,
    ) -> Result<(), ManagerError> {
        if self.states.get(unit_name).is_some_and(ServiceState::is_active) {
            log::debug!("{} already running, skipping", unit_name);
            return Ok(());
        }

        match self.start_single(unit_name).await {
            Ok(()) => {
                started.push(unit_name.to_string());
                Ok(())
            }
            Err(ManagerError::IsTarget(_)) => {
                if let Some(state) = self.states.get_mut(unit_name) {
                    state.set_running(0);
                }
                log::debug!("Target {} reached", unit_name);
                Ok(())
            }
            Err(e) => self.handle_dependency_start_error(root_name, unit_name, e),
        }
    }

    fn handle_dependency_start_error(
        &self,
        root_name: &str,
        unit_name: &str,
        err: ManagerError,
    ) -> Result<(), ManagerError> {
        let is_required = self
            .units
            .get(root_name)
            .map(|u| u.unit_section().requires.contains(&unit_name.to_string()))
            .unwrap_or(false);
        if is_required {
            log::error!("Required dependency {} failed: {}", unit_name, err);
            return Err(err);
        }
        log::warn!("Optional dependency {} failed: {}", unit_name, err);
        Ok(())
    }

    async fn ensure_unit_loaded(&mut self, name: &str) -> Result<(), ManagerError> {
        if self.units.contains_key(name) {
            return Ok(());
        }
        let _ = self.load(name).await?;
        Ok(())
    }

    async fn collect_start_dependencies(
        &mut self,
        name: &str,
    ) -> (HashSet<String>, HashMap<String, String>) {
        let mut to_load: Vec<String> = vec![name.to_string()];
        let mut queued: HashSet<String> = [name.to_string()].into_iter().collect();
        let mut loaded: HashSet<String> = HashSet::new();
        let mut aliases: HashMap<String, String> = HashMap::new();

        while let Some(unit_name) = to_load.pop() {
            if loaded.contains(&unit_name) || aliases.contains_key(&unit_name) {
                continue;
            }
            let Some(actual_name) = self.load_dependency_unit(&unit_name).await else {
                continue;
            };
            if unit_name != actual_name {
                aliases.insert(unit_name.clone(), actual_name.clone());
            }
            loaded.insert(actual_name.clone());
            self.queue_unit_dependencies(&actual_name, &mut to_load, &mut queued);
        }

        (loaded, aliases)
    }

    async fn load_dependency_unit(&mut self, unit_name: &str) -> Option<String> {
        if self.units.contains_key(unit_name) {
            return Some(unit_name.to_string());
        }
        match self.load(unit_name).await {
            Ok(canonical) => Some(canonical),
            Err(e) => {
                log::warn!("Could not load dependency {}: {}", unit_name, e);
                None
            }
        }
    }

    fn queue_unit_dependencies(
        &self,
        actual_name: &str,
        to_load: &mut Vec<String>,
        queued: &mut HashSet<String>,
    ) {
        let Some(unit) = self.units.get(actual_name) else {
            return;
        };

        let section = unit.unit_section();
        if !section.requires.is_empty() || !section.wants.is_empty() || !unit.wants_dir().is_empty() {
            log::debug!(
                "{}: Requires={:?}, Wants={:?}, wants_dir={:?}",
                actual_name,
                section.requires,
                section.wants,
                unit.wants_dir()
            );
        }

        for dep in &section.requires {
            queue_dependency(to_load, queued, dep);
        }
        for dep in &section.wants {
            queue_dependency(to_load, queued, dep);
        }
        for dep in unit.wants_dir() {
            queue_dependency(to_load, queued, dep);
        }
    }

    fn build_start_graph(
        &self,
        loaded: &HashSet<String>,
        aliases: &HashMap<String, String>,
    ) -> deps::DepGraph {
        let mut graph = deps::DepGraph::new();
        for (alias, canonical) in aliases {
            graph.add_alias(alias, canonical);
        }
        for key in self.units.keys().filter(|key| loaded.contains(*key)) {
            graph.add_node(key);
        }
        for (key, unit) in self.units.iter().filter(|(key, _)| loaded.contains(*key)) {
            graph.add_unit_with_name(key, unit);
        }
        graph
    }

    fn log_start_single_request(&self, name: &str) {
        log::debug!("start_single({})", name);
        if name.contains("dbus") {
            log::info!(
                ">>> start_single({}) - socket_fds keys: {:?}",
                name,
                self.socket_fds.keys().collect::<Vec<_>>()
            );
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempRoot(PathBuf);

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    static TEMP_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(label: &str) -> TempRoot {
        let id = TEMP_ID.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "sysd-manager-{label}-{}-{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        TempRoot(path)
    }

    fn write_unit(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn constructors_initialize_mode_specific_channels_and_paths() {
        let system = Manager::new();
        assert!(!system.is_user_mode());
        assert!(system.socket_activation_rx.is_some());
        assert!(system.timer_rx.is_some());
        assert!(system.path_rx.is_some());
        assert!(system.oneshot_completion_rx.is_some());
        assert_eq!(system.enable_dir(), PathBuf::from("/etc/systemd/system"));
        assert_eq!(system.notify_socket_path_for_mode(), NOTIFY_SOCKET_PATH);
        assert!(system.notify_socket_path().is_none());

        let user = Manager::new_user();
        assert!(user.is_user_mode());
        assert!(!user.cgroups_available());
        assert!(user
            .notify_socket_path_for_mode()
            .starts_with("/run/user/"));
        assert!(!user.executor_path.is_empty());
    }

    #[test]
    fn search_unit_paths_find_unit_and_missing_unit_errors() {
        let dir = temp_dir("find-unit");
        write_unit(
            &dir.0,
            "demo.service",
            r#"
[Service]
ExecStart=/bin/true
"#,
        );
        let mut manager = Manager::new();
        manager.unit_paths = vec![dir.0.clone()];

        assert_eq!(
            manager.search_unit_paths("demo.service"),
            Some(dir.0.join("demo.service"))
        );
        assert_eq!(
            manager.find_unit("demo.service").unwrap(),
            dir.0.join("demo.service")
        );
        assert!(matches!(
            manager.find_unit("missing.service"),
            Err(ManagerError::NotFound(name)) if name == "missing.service"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn canonical_unit_name_resolves_symlink_targets_with_unit_names() {
        use std::os::unix::fs::symlink;

        let dir = temp_dir("canonical");
        let real = write_unit(
            &dir.0,
            "real.service",
            r#"
[Service]
ExecStart=/bin/true
"#,
        );
        let alias = dir.0.join("alias.service");
        symlink(&real, &alias).unwrap();
        let manager = Manager::new();

        assert_eq!(
            manager
                .resolve_canonical_unit_name("alias.service", &alias)
                .unwrap(),
            "real.service"
        );
        assert_eq!(
            manager
                .resolve_canonical_unit_name("real.service", &real)
                .unwrap(),
            "real.service"
        );
    }

    #[test]
    fn apply_canonical_name_updates_wrapped_unit_names() {
        let manager = Manager::new();
        let mut unit = Unit::Service(Service::new("template@.service".to_string()));

        manager.apply_canonical_name(&mut unit, "template@demo.service");

        assert_eq!(unit.name(), "template@demo.service");
        assert_eq!(unit.as_service().unwrap().instance.as_deref(), Some("demo"));
    }

    #[tokio::test]
    async fn load_from_path_parses_unit_and_initializes_state() {
        let dir = temp_dir("load-path");
        let unit_path = write_unit(
            &dir.0,
            "demo.service",
            r#"
[Unit]
Description=Demo

[Service]
ExecStart=/bin/true
"#,
        );
        let mut manager = Manager::new();

        manager.load_from_path(&unit_path).await.unwrap();

        assert!(manager.units.contains_key("demo.service"));
        assert!(manager.states.contains_key("demo.service"));
    }
}
