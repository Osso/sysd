use super::*;
use crate::units::{DevicePolicy, ProtectHome, ProtectProc, ProtectSystem, ServiceSection};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static TEMP_ID: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir(label: &str) -> TempDir {
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("sysd-sandbox-{label}-{id}"));
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path)
}

#[test]
fn mount_namespace_predicate_tracks_every_mount_related_setting() {
    let base = ServiceSection::default();
    assert!(!needs_mount_namespace(&base));

    let cases: &[Box<dyn Fn(&mut ServiceSection)>] = &[
        Box::new(|service| service.protect_system = ProtectSystem::Yes),
        Box::new(|service| service.protect_home = ProtectHome::ReadOnly),
        Box::new(|service| service.private_tmp = true),
        Box::new(|service| service.private_devices = true),
        Box::new(|service| service.device_policy = DevicePolicy::Closed),
        Box::new(|service| service.protect_proc = ProtectProc::Invisible),
        Box::new(|service| service.read_only_paths = vec![PathBuf::from("/usr")]),
        Box::new(|service| service.read_write_paths = vec![PathBuf::from("/var")]),
        Box::new(|service| service.inaccessible_paths = vec![PathBuf::from("/secret")]),
        Box::new(|service| service.protect_control_groups = true),
        Box::new(|service| service.protect_kernel_tunables = true),
        Box::new(|service| service.protect_kernel_logs = true),
    ];

    for configure in cases {
        let mut service = ServiceSection::default();
        configure(&mut service);
        assert!(needs_mount_namespace(&service));
    }
}

#[test]
fn seccomp_predicate_tracks_all_seccomp_related_settings() {
    let base = ServiceSection::default();
    assert!(!has_seccomp_settings(&base));

    let cases: &[Box<dyn Fn(&mut ServiceSection)>] = &[
        Box::new(|service| service.restrict_namespaces = Some(vec![])),
        Box::new(|service| service.system_call_filter = vec!["@system-service".to_string()]),
        Box::new(|service| service.protect_clock = true),
        Box::new(|service| service.protect_hostname = true),
        Box::new(|service| service.restrict_suid_sgid = true),
        Box::new(|service| service.restrict_address_families = Some(vec!["AF_UNIX".to_string()])),
        Box::new(|service| service.system_call_architectures = vec!["native".to_string()]),
    ];

    for configure in cases {
        let mut service = ServiceSection::default();
        configure(&mut service);
        assert!(has_seccomp_settings(&service));
    }
}

#[test]
fn capability_names_accept_prefixes_and_case_variants() {
    assert_eq!(
        Capability::from_name("CAP_SYS_ADMIN").map(|cap| cap as u32),
        Some(Capability::SysAdmin as u32)
    );
    assert_eq!(
        Capability::from_name("sys_admin").map(|cap| cap as u32),
        Some(Capability::SysAdmin as u32)
    );
    assert_eq!(
        Capability::from_name("Net_Bind_Service").map(|cap| cap as u32),
        Some(Capability::NetBindService as u32)
    );
    assert!(Capability::from_name("CAP_DOES_NOT_EXIST").is_none());
}

#[test]
fn capability_lists_tolerate_unknown_capabilities_without_failing() {
    assert_eq!(
        apply_capability_bounding_set(&["CAP_DOES_NOT_EXIST".to_string()]),
        Ok(())
    );
    assert_eq!(
        apply_ambient_capabilities(&["CAP_DOES_NOT_EXIST".to_string()]),
        Ok(())
    );
}

#[test]
fn device_allow_parser_tolerates_empty_unknown_and_missing_device_entries() {
    assert_eq!(add_device_allow_entry(""), Ok(()));
    assert_eq!(add_device_allow_entry("not-a-device rw"), Ok(()));
    assert_eq!(add_device_allow_entry("/dev/definitely-missing-sysd-test r"), Ok(()));
    assert_eq!(add_device_allow_entry("block-unsupported r"), Ok(()));
}

#[test]
fn device_node_detection_distinguishes_regular_files_from_missing_paths() {
    let root = temp_dir("device-node");
    let regular_file = root.0.join("regular");
    std::fs::write(&regular_file, "not a device").unwrap();

    assert_eq!(is_device_node(&regular_file), Ok(false));
    assert!(is_device_node(&root.0.join("missing")).is_err());
}

#[test]
fn ensure_device_placeholder_creates_parent_directories_and_file() {
    let root = temp_dir("placeholder");
    let placeholder = root.0.join("nested/device");

    ensure_device_placeholder(placeholder.to_str().unwrap());

    assert!(placeholder.exists());
    assert!(placeholder.is_file());
}

#[test]
fn no_op_sandbox_paths_accept_default_service() {
    let service = ServiceSection::default();

    assert_eq!(apply_sandbox(&service), Ok(()));
    assert_eq!(apply_basic_sandbox_settings(&service), Ok(()));
    assert_eq!(apply_prctl_settings(&service), Ok(()));
    assert_eq!(apply_device_namespace_policy(&service), Ok(()));
    assert_eq!(apply_mount_protections(&service), Ok(()));
    assert_eq!(apply_protect_proc(&ProtectProc::Default), Ok(()));
}
