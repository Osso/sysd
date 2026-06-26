use super::*;
use crate::manager::{ActiveState, Manager, SubState};
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;
use tokio::sync::RwLock;
use zbus::zvariant::{Array, Fd, OwnedValue, Str, Type, Value};

fn string_value(value: &'static str) -> OwnedValue {
    OwnedValue::from(Str::from_static(value))
}

fn u32_array_value(values: &[u32]) -> OwnedValue {
    let mut array = Array::new(<u32 as Type>::SIGNATURE);
    for value in values {
        array.append(Value::U32(*value)).unwrap();
    }
    OwnedValue::try_from(Value::Array(array)).unwrap()
}

fn fd_array_value(owned_fd: OwnedFd) -> OwnedValue {
    let mut array = Array::new(<Fd<'_> as Type>::SIGNATURE);
    array.append(Value::Fd(Fd::from(owned_fd))).unwrap();
    OwnedValue::try_from(Value::Array(array)).unwrap()
}

fn pidfd_array_value() -> OwnedValue {
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, std::process::id(), 0) };
    assert_ne!(pidfd, -1, "pidfd_open should succeed for current process");
    let owned_fd = unsafe { OwnedFd::from_raw_fd(pidfd as i32) };
    fd_array_value(owned_fd)
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(label: &str) -> TempDir {
    let id = format!("{}-{}", std::process::id(), next_job_id());
    let path = std::env::temp_dir().join(format!("sysd-dbus-manager-{label}-{id}"));
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path)
}

#[test]
fn job_ids_and_paths_are_monotonic_and_systemd_shaped() {
    let first = next_job_id();
    let second = next_job_id();

    assert_eq!(second, first + 1);
    assert_eq!(
        job_path(first).as_str(),
        format!("/org/freedesktop/systemd1/job/{first}")
    );
}

#[test]
fn special_user_unit_detection_routes_only_user_units() {
    assert_eq!(start_special_user_unit("not-special.service"), None,);
    assert_eq!(
        start_special_user_unit("user-runtime-dir@invalid.service"),
        Some("failed")
    );
    assert_eq!(
        start_special_user_unit("user@invalid.service"),
        Some("failed")
    );
}

#[test]
fn user_runtime_dir_unit_accepts_current_user() {
    let uid = unsafe { libc::geteuid() };
    let unit = format!("user-runtime-dir@{uid}.service");

    assert_eq!(start_user_runtime_dir(&unit), "done");
}

#[test]
fn user_session_bus_reports_spawn_error_for_invalid_paths() {
    assert!(!ensure_user_session_bus(
        unsafe { libc::geteuid() },
        "runtime\0dir",
        "bus\0path"
    ));
}

#[test]
fn parse_uid_from_unit_accepts_expected_suffix_and_rejects_invalid_values() {
    assert_eq!(
        parse_uid_from_unit("user-runtime-dir@1000.service", USER_RUNTIME_DIR_PREFIX),
        Some(1000)
    );
    assert_eq!(
        parse_uid_from_unit("user@0.service", USER_MANAGER_PREFIX),
        Some(0)
    );
    assert_eq!(
        parse_uid_from_unit("user-runtime-dir@abc.service", USER_RUNTIME_DIR_PREFIX),
        None
    );
    assert_eq!(
        parse_uid_from_unit("user-runtime-dir@1000.socket", USER_RUNTIME_DIR_PREFIX),
        None
    );
}

#[test]
fn parse_string_property_only_accepts_string_values() {
    assert_eq!(
        parse_string_property(&string_value("user.slice")).as_deref(),
        Some("user.slice")
    );
    assert_eq!(parse_string_property(&OwnedValue::from(12_u32)), None);
}

#[test]
fn collect_u32_array_extends_with_only_u32_entries() {
    let mut pids = vec![1];
    collect_u32_array(&u32_array_value(&[10, 20]), &mut pids);

    assert_eq!(pids, [1, 10, 20]);

    collect_u32_array(&string_value("not-array"), &mut pids);
    assert_eq!(pids, [1, 10, 20]);
}

#[test]
fn parse_scope_properties_collects_slice_description_and_pids() {
    let properties = vec![
        ("Slice".to_string(), string_value("user-1000.slice")),
        ("Description".to_string(), string_value("Session 7")),
        ("PIDs".to_string(), u32_array_value(&[100, 101])),
        ("Ignored".to_string(), string_value("value")),
    ];

    let (slice, description, pids) = parse_scope_properties(&properties);

    assert_eq!(slice.as_deref(), Some("user-1000.slice"));
    assert_eq!(description.as_deref(), Some("Session 7"));
    assert_eq!(pids, [100, 101]);
}

#[test]
fn parse_scope_properties_defaults_when_properties_are_missing_or_wrong_type() {
    let properties = vec![
        ("Slice".to_string(), OwnedValue::from(42_u32)),
        ("Description".to_string(), u32_array_value(&[1, 2])),
        ("PIDs".to_string(), string_value("not-array")),
        ("PIDFDs".to_string(), u32_array_value(&[99])),
    ];

    let (slice, description, pids) = parse_scope_properties(&properties);

    assert_eq!(slice, None);
    assert_eq!(description, None);
    assert!(pids.is_empty());
}

#[test]
fn parse_scope_properties_collects_pidfds() {
    let properties = vec![("PIDFDs".to_string(), pidfd_array_value())];

    let (slice, description, pids) = parse_scope_properties(&properties);

    assert_eq!(slice, None);
    assert_eq!(description, None);
    assert_eq!(pids, [std::process::id()]);
}

#[test]
fn parse_scope_properties_ignores_fd_values_that_are_not_pidfds() {
    let root = temp_dir("plain-fd");
    let file = std::fs::File::create(root.0.join("plain-fd")).unwrap();
    let properties = vec![("PIDFDs".to_string(), fd_array_value(file.into()))];

    let (slice, description, pids) = parse_scope_properties(&properties);

    assert_eq!(slice, None);
    assert_eq!(description, None);
    assert!(pids.is_empty());
}

#[test]
fn pidfd_to_pid_reports_missing_fd() {
    let error = pidfd_to_pid(-1).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn pidfd_to_pid_reports_current_process_for_pidfd() {
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, std::process::id(), 0) };
    assert_ne!(pidfd, -1, "pidfd_open should succeed for current process");

    let pid = pidfd_to_pid(pidfd as std::os::unix::io::RawFd).unwrap();
    unsafe { libc::close(pidfd as i32) };

    assert_eq!(pid, std::process::id());
}

#[test]
fn ensure_user_session_bus_accepts_existing_bus_path() {
    let root = temp_dir("existing-bus");
    let bus_path = root.0.join("bus");
    std::fs::write(&bus_path, b"").unwrap();

    assert!(ensure_user_session_bus(
        1234,
        root.0.to_str().unwrap(),
        bus_path.to_str().unwrap()
    ));
}

#[test]
fn start_user_sysd_reports_failed_when_runtime_env_is_invalid() {
    assert_eq!(
        start_user_sysd(
            unsafe { libc::geteuid() },
            "runtime\0dir",
            "/tmp/sysd-test-sysd.sock",
            "/tmp/sysd-test-bus",
        ),
        "failed"
    );
}

#[test]
fn log_scope_start_accepts_all_optional_inputs() {
    log_scope_start(
        "session-3.scope",
        "replace",
        Some("user-1000.slice"),
        Some("Session 3"),
        &[std::process::id()],
    );

    log_scope_start("session-empty.scope", "fail", None, None, &[]);
}

#[tokio::test]
async fn manager_interface_reports_static_paths_and_version() {
    let interface = ManagerInterface::new(Arc::new(RwLock::new(Manager::new_user())));

    assert_eq!(interface.version().await, "sysd 0.1.0");
    assert_eq!(
        interface.get_unit("sshd.service").await.unwrap().as_str(),
        "/org/freedesktop/systemd1/unit/sshd_2eservice"
    );
    assert_eq!(
        interface
            .load_unit("session-2.scope")
            .await
            .unwrap()
            .as_str(),
        "/org/freedesktop/systemd1/unit/session_2d2_2escope"
    );
    assert_eq!(interface.subscribe().await, Ok(()));
    assert_eq!(interface.reload().await, Ok(()));
}

#[tokio::test]
async fn stop_unit_returns_job_path_even_when_unit_is_missing() {
    let interface = ManagerInterface::new(Arc::new(RwLock::new(Manager::new_user())));

    let job = interface
        .stop_unit("definitely-missing.service", "replace")
        .await
        .unwrap();

    assert!(job.as_str().starts_with("/org/freedesktop/systemd1/job/"));
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

#[tokio::test]
async fn start_unit_and_transient_unit_return_job_paths_with_signal_context() {
    let Ok(conn) = zbus::Connection::session().await else {
        return;
    };
    let ctx = zbus::object_server::SignalEmitter::new(&conn, "/org/freedesktop/systemd1").unwrap();
    let manager = Arc::new(RwLock::new(Manager::new_user()));
    let interface = ManagerInterface::new(Arc::clone(&manager));

    let start_job = interface
        .start_unit(ctx.clone(), "definitely-missing.service", "replace")
        .await
        .unwrap();
    assert!(start_job
        .as_str()
        .starts_with("/org/freedesktop/systemd1/job/"));

    let transient_job = interface
        .start_transient_unit(
            ctx,
            "session-signal.scope",
            "replace",
            vec![
                ("Description".to_string(), string_value("Signal session")),
                ("PIDs".to_string(), u32_array_value(&[std::process::id()])),
            ],
            Vec::new(),
        )
        .await
        .unwrap();
    assert!(transient_job
        .as_str()
        .starts_with("/org/freedesktop/systemd1/job/"));

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(manager
        .read()
        .await
        .scope_manager()
        .exists("session-signal.scope"));
}

#[tokio::test]
async fn signal_helpers_emit_job_and_unit_removed_when_session_bus_is_available() {
    let Ok(conn) = zbus::Connection::session().await else {
        return;
    };
    let ctx = zbus::object_server::SignalEmitter::new(&conn, "/org/freedesktop/systemd1").unwrap();

    ManagerInterface::emit_job_removed(&ctx, 42, "demo.service", "done")
        .await
        .unwrap();
    ManagerInterface::emit_unit_removed(&ctx, "demo.service")
        .await
        .unwrap();
    emit_job_removed_signal(&conn, 43, "demo.service", "failed", "Test").await;
}

#[tokio::test]
async fn kill_unit_ignores_missing_units_and_checks_scope_main_pid() {
    let manager = Arc::new(RwLock::new(Manager::new_user()));
    let interface = ManagerInterface::new(Arc::clone(&manager));

    interface
        .kill_unit("definitely-missing.scope", "all", 0)
        .await
        .unwrap();

    assert_eq!(
        register_scope_job(
            Arc::clone(&manager),
            "session-kill.scope",
            None,
            Some("Kill test"),
            &[std::process::id()],
        )
        .await,
        "done"
    );

    interface
        .kill_unit("session-kill.scope", "all", 0)
        .await
        .unwrap();
}

#[tokio::test]
async fn start_regular_unit_reports_failed_for_missing_units() {
    let manager = Arc::new(RwLock::new(Manager::new_user()));

    let result = start_regular_unit(manager, "definitely-missing.service").await;

    assert_eq!(result, "failed");
}

#[tokio::test]
async fn resolve_start_unit_result_routes_special_and_regular_units() {
    let manager = Arc::new(RwLock::new(Manager::new_user()));

    assert_eq!(
        resolve_start_unit_result(Arc::clone(&manager), "user-runtime-dir@bad.service").await,
        "failed"
    );
    assert_eq!(
        resolve_start_unit_result(manager, "definitely-missing.service").await,
        "failed"
    );
}

#[tokio::test]
async fn register_scope_job_tracks_active_scope_with_defaults() {
    let manager = Arc::new(RwLock::new(Manager::new_user()));

    let result = register_scope_job(
        Arc::clone(&manager),
        "session-55.scope",
        None,
        Some("Session 55"),
        &[std::process::id()],
    )
    .await;

    assert_eq!(result, "done");
    let manager = manager.read().await;
    let state = manager
        .list()
        .find_map(|(name, state)| (name == "session-55.scope").then_some(state))
        .unwrap();
    assert_eq!(state.active, ActiveState::Active);
    assert_eq!(state.sub, SubState::Running);
    assert!(manager.scope_manager().exists("session-55.scope"));
    assert_eq!(
        manager
            .scope_manager()
            .get_cgroup_path("session-55.scope")
            .unwrap()
            .to_string_lossy(),
        "/sys/fs/cgroup/user.slice/session-55.scope"
    );
}

#[tokio::test]
async fn register_scope_job_uses_explicit_slice() {
    let manager = Arc::new(RwLock::new(Manager::new_user()));

    let result = register_scope_job(
        Arc::clone(&manager),
        "session-77.scope",
        Some("user-1000.slice"),
        None,
        &[],
    )
    .await;

    assert_eq!(result, "done");
    let manager = manager.read().await;
    assert_eq!(
        manager
            .scope_manager()
            .get_cgroup_path("session-77.scope")
            .unwrap()
            .to_string_lossy(),
        "/sys/fs/cgroup/user-1000.slice/session-77.scope"
    );
}
