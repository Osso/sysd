use super::*;
use crate::manager::{ActiveState, Manager, SubState};
use std::sync::Arc;
use tokio::sync::RwLock;
use zbus::zvariant::{Array, OwnedValue, Str, Type, Value};

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
fn pidfd_to_pid_reports_missing_fd() {
    let error = pidfd_to_pid(-1).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
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
