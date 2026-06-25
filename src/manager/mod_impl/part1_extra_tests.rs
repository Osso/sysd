use super::*;
use crate::units::Target;
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
        "sysd-manager-extra-{label}-{}-{id}",
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

#[tokio::test]
async fn load_from_path_reports_parse_errors() {
    let dir = temp_dir("load-path-error");
    let unit_path = dir.0.join("missing.service");
    let mut manager = Manager::new_user();

    assert!(matches!(
        manager.load_from_path(&unit_path).await,
        Err(ManagerError::Parse(message)) if message.contains("No such file")
    ));
    assert!(manager.units.is_empty());
    assert!(manager.states.is_empty());
}

#[tokio::test]
async fn load_bare_template_uses_default_instance_when_available() {
    let dir = temp_dir("template-default");
    write_unit(
        &dir.0,
        "worker@.service",
        r#"
[Service]
ExecStart=/bin/true

[Install]
DefaultInstance=blue
"#,
    );
    let mut manager = Manager::new_user();
    manager.unit_paths = vec![dir.0.clone()];

    let loaded = manager.load("worker@.service").await.unwrap();

    assert_eq!(loaded, "worker@blue.service");
    assert!(manager.units.contains_key("worker@blue.service"));
    assert!(!manager.units.contains_key("worker@.service"));
}

#[cfg(unix)]
#[test]
fn canonical_unit_name_rejects_masked_units() {
    use std::os::unix::fs::symlink;

    let dir = temp_dir("masked");
    let masked = dir.0.join("masked.service");
    symlink("/dev/null", &masked).unwrap();
    let manager = Manager::new_user();

    assert!(matches!(
        manager.resolve_canonical_unit_name("masked.service", &masked),
        Err(ManagerError::Masked(name)) if name == "masked.service"
    ));
}

#[tokio::test]
async fn dependency_collection_loads_available_units_and_ignores_missing_optional_units() {
    let dir = temp_dir("deps");
    write_unit(
        &dir.0,
        "app.service",
        r#"
[Unit]
Requires=db.service
Wants=missing.service

[Service]
ExecStart=/bin/true
"#,
    );
    write_unit(
        &dir.0,
        "db.service",
        r#"
[Service]
ExecStart=/bin/true
"#,
    );
    let mut manager = Manager::new_user();
    manager.unit_paths = vec![dir.0.clone()];

    let (loaded, aliases) = manager.collect_start_dependencies("app.service").await;

    assert!(loaded.contains("app.service"));
    assert!(loaded.contains("db.service"));
    assert!(!loaded.contains("missing.service"));
    assert!(aliases.is_empty());
    let graph = manager.build_start_graph(&loaded, &aliases);
    let order = graph.start_order_for("app.service").unwrap();
    assert!(
        order.iter().position(|name| name == "db.service").unwrap()
            < order.iter().position(|name| name == "app.service").unwrap()
    );
}

#[test]
fn queue_unit_dependencies_reads_requires_wants_and_target_wants_dir() {
    let mut manager = Manager::new_user();
    let mut target = Target::new("multi-user.target".to_string());
    target.unit.requires = vec!["db.service".to_string()];
    target.unit.wants = vec!["log.service".to_string()];
    target.wants_dir = vec!["ssh.service".to_string(), "db.service".to_string()];
    manager
        .units
        .insert("multi-user.target".to_string(), Unit::Target(target));
    let mut to_load = Vec::new();
    let mut queued = HashSet::new();

    manager.queue_unit_dependencies("multi-user.target", &mut to_load, &mut queued);
    manager.queue_unit_dependencies("missing.target", &mut to_load, &mut queued);

    assert_eq!(to_load, ["db.service", "log.service", "ssh.service"]);
    assert_eq!(queued.len(), 3);
}

#[tokio::test]
async fn start_dependency_unit_skips_active_units_and_marks_targets_active() {
    let mut manager = Manager::new_user();
    manager.units.insert(
        "root.service".to_string(),
        Unit::Service(Service::new("root.service".to_string())),
    );
    manager.units.insert(
        "active.service".to_string(),
        Unit::Service(Service::new("active.service".to_string())),
    );
    manager.units.insert(
        "ready.target".to_string(),
        Unit::Target(Target::new("ready.target".to_string())),
    );
    manager
        .states
        .insert("active.service".to_string(), ServiceState::new());
    manager
        .states
        .get_mut("active.service")
        .unwrap()
        .set_running(7);
    manager
        .states
        .insert("ready.target".to_string(), ServiceState::new());
    let mut started = Vec::new();

    manager
        .start_dependency_unit("root.service", "active.service", &mut started)
        .await
        .unwrap();
    manager
        .start_dependency_unit("root.service", "ready.target", &mut started)
        .await
        .unwrap();

    assert!(started.is_empty());
    assert!(manager.states.get("ready.target").unwrap().is_active());
}

#[test]
fn dependency_start_errors_fail_required_units_and_ignore_optional_units() {
    let mut manager = Manager::new_user();
    let mut root = Service::new("root.service".to_string());
    root.unit.requires = vec!["required.service".to_string()];
    root.unit.wants = vec!["optional.service".to_string()];
    manager
        .units
        .insert("root.service".to_string(), Unit::Service(root));

    assert!(matches!(
        manager.handle_dependency_start_error(
            "root.service",
            "required.service",
            ManagerError::NotFound("required.service".to_string()),
        ),
        Err(ManagerError::NotFound(name)) if name == "required.service"
    ));
    assert!(manager
        .handle_dependency_start_error(
            "root.service",
            "optional.service",
            ManagerError::NotFound("optional.service".to_string()),
        )
        .is_ok());
}
