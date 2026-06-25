use super::*;
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
fn pidfd_to_pid_reports_missing_fd() {
    let error = pidfd_to_pid(-1).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
