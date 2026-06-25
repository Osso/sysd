use super::*;
use crate::units::{DevicePolicy, ProtectHome, ProtectProc, ProtectSystem, ServiceSection};
use std::collections::BTreeMap;
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

#[test]
fn path_restrictions_ignore_missing_paths_without_mounting() {
    let missing = PathBuf::from(format!(
        "/tmp/sysd-sandbox-missing-{}",
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));

    assert_eq!(
        apply_path_restrictions(&[missing.clone()], &[missing.clone()], &[missing]),
        Ok(())
    );
}

#[test]
fn namespace_restriction_defaults_to_all_known_namespaces() {
    let all_flags = [
        ("cgroup", libc::CLONE_NEWCGROUP as u64),
        ("ipc", libc::CLONE_NEWIPC as u64),
        ("net", libc::CLONE_NEWNET as u64),
        ("mnt", libc::CLONE_NEWNS as u64),
        ("pid", libc::CLONE_NEWPID as u64),
        ("user", libc::CLONE_NEWUSER as u64),
        ("uts", libc::CLONE_NEWUTS as u64),
    ];
    let default_flags = blocked_namespace_flags(&[], &all_flags);
    let selected_flags = blocked_namespace_flags(&["Net".to_string(), "USER".to_string()], &all_flags);
    let unknown_flags = blocked_namespace_flags(&["unknown".to_string()], &all_flags);

    assert_eq!(default_flags.len(), all_flags.len());
    assert_eq!(
        selected_flags,
        vec![libc::CLONE_NEWNET as u64, libc::CLONE_NEWUSER as u64]
    );
    assert!(unknown_flags.is_empty());
}

#[test]
fn namespace_rules_block_unshare_clone_and_clone3() {
    let mut rules = rule_map();

    add_restrict_namespaces_rules(&mut rules, &["net".to_string()]).unwrap();

    assert!(rules.contains_key(&(libc::SYS_unshare as i64)));
    assert!(rules.contains_key(&(libc::SYS_clone as i64)));
    assert!(rules.contains_key(&435));
}

#[test]
fn syscall_filters_only_add_deny_rules_for_known_syscalls() {
    let mut rules = rule_map();

    add_syscall_filter_rules(
        &mut rules,
        &[
            "mount".to_string(),
            "~mount".to_string(),
            "~@clock".to_string(),
            "~does_not_exist".to_string(),
            "@clock".to_string(),
        ],
    )
    .unwrap();

    assert!(rules.contains_key(&syscall_name_to_nr("mount").unwrap()));
    assert!(rules.contains_key(&syscall_name_to_nr("clock_settime").unwrap()));
    assert!(rules.contains_key(&syscall_name_to_nr("clock_adjtime").unwrap()));
    assert!(rules.contains_key(&syscall_name_to_nr("settimeofday").unwrap()));
}

#[test]
fn syscall_group_lookup_returns_known_groups_and_empty_unknown_group() {
    assert_eq!(get_syscall_group("swap"), ["swapon", "swapoff"]);
    assert_eq!(
        get_syscall_group("mount"),
        ["mount", "umount", "umount2", "pivot_root", "move_mount"]
    );
    assert!(get_syscall_group("not-a-group").is_empty());
}

#[test]
fn combined_seccomp_collection_merges_all_supported_toggles() {
    let mut service = ServiceSection {
        restrict_namespaces: Some(vec!["net".to_string()]),
        system_call_filter: vec!["~@clock".to_string()],
        restrict_realtime: true,
        protect_clock: true,
        protect_hostname: true,
        lock_personality: true,
        restrict_suid_sgid: true,
        restrict_address_families: Some(vec!["~AF_INET".to_string(), "~AF_INET6".to_string()]),
        ..Default::default()
    };
    let mut rules = rule_map();

    collect_combined_seccomp_rules(&service, &mut rules).unwrap();

    assert!(rules.contains_key(&(libc::SYS_unshare as i64)));
    assert!(rules.contains_key(&syscall_name_to_nr("clock_settime").unwrap()));
    assert!(rules.contains_key(&syscall_name_to_nr("sethostname").unwrap()));
    assert!(rules.contains_key(&personality_syscall_nr()));
    assert!(rules.contains_key(&socket_syscall_nr()));
    assert!(rules.len() >= 10);

    service.system_call_filter.clear();
    let before = rules.len();
    collect_combined_seccomp_rules(&service, &mut rules).unwrap();
    assert!(rules.len() >= before);
}

#[test]
fn address_family_rules_only_block_tilde_prefixed_deny_entries() {
    let mut deny_rules = rule_map();
    add_restrict_address_families_rules(
        &mut deny_rules,
        &[
            "~AF_UNIX".to_string(),
            "~af_inet".to_string(),
            "~AF_UNKNOWN".to_string(),
        ],
    )
    .unwrap();

    let socket_rules = deny_rules.get(&socket_syscall_nr()).unwrap();
    assert_eq!(socket_rules.len(), 2);

    let mut allow_rules = rule_map();
    add_restrict_address_families_rules(&mut allow_rules, &["AF_UNIX".to_string()]).unwrap();
    assert!(allow_rules.is_empty());
}

#[test]
fn protection_rule_helpers_add_expected_syscall_entries() {
    let mut rules = rule_map();

    add_restrict_realtime_rules(&mut rules).unwrap();
    add_protect_clock_rules(&mut rules).unwrap();
    add_protect_hostname_rules(&mut rules).unwrap();
    add_lock_personality_rules(&mut rules).unwrap();
    add_restrict_suid_sgid_rules(&mut rules).unwrap();

    assert!(rules.contains_key(&syscall_name_to_nr("clock_settime").unwrap()));
    assert!(rules.contains_key(&syscall_name_to_nr("settimeofday").unwrap()));
    assert!(rules.contains_key(&syscall_name_to_nr("sethostname").unwrap()));
    assert!(rules.contains_key(&syscall_name_to_nr("setdomainname").unwrap()));
    assert!(rules.contains_key(&personality_syscall_nr()));
    assert!(rules.len() >= 10);
}

#[test]
fn syscall_lookup_matches_native_table_and_unknowns_return_none() {
    assert_eq!(lookup_syscall_nr(&[("one", 1), ("two", 2)], "two"), Some(2));
    assert_eq!(lookup_syscall_nr(&[("one", 1)], "missing"), None);
    assert_eq!(syscall_name_to_nr("mount"), native_mount_syscall_nr());
    assert_eq!(syscall_name_to_nr("definitely_not_a_syscall"), None);
    assert!(native_seccomp_arch().is_some());
}

#[test]
fn native_syscall_table_contains_late_security_entries() {
    assert!(syscall_name_to_nr("kexec_file_load").is_some());
    assert!(syscall_name_to_nr("open_tree").is_some());
    assert!(syscall_name_to_nr("move_mount").is_some());
    assert!(syscall_name_to_nr("bpf").is_some());
}

#[test]
fn prctl_backed_part3_helpers_are_callable_without_mounts() {
    assert_eq!(apply_restrict_realtime(), Ok(()));
    assert_eq!(apply_lock_personality(), Ok(()));
}

#[test]
fn ignore_sigpipe_sets_sigpipe_to_ignore() {
    let original = unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    unsafe { libc::signal(libc::SIGPIPE, original) };

    assert_eq!(apply_ignore_sigpipe(), Ok(()));

    let previous = unsafe { libc::signal(libc::SIGPIPE, original) };

    assert_eq!(previous, libc::SIG_IGN);
}

#[test]
fn mount_helpers_reject_paths_with_nul_before_mounting() {
    assert!(bind_mount_ro("bad\0path").unwrap_err().contains("nul byte"));
    assert!(remount_rw("bad\0path").unwrap_err().contains("nul byte"));
    assert!(mount_tmpfs("bad\0path").unwrap_err().contains("nul byte"));
    assert!(make_inaccessible("bad\0path")
        .unwrap_err()
        .contains("nul byte"));
    assert!(remount_proc("hidepid=2\0bad")
        .unwrap_err()
        .contains("nul byte"));
}

fn rule_map() -> BTreeMap<i64, Vec<SeccompRule>> {
    BTreeMap::new()
}

#[cfg(target_arch = "x86_64")]
fn socket_syscall_nr() -> i64 {
    41
}

#[cfg(target_arch = "aarch64")]
fn socket_syscall_nr() -> i64 {
    198
}

#[cfg(target_arch = "x86_64")]
fn personality_syscall_nr() -> i64 {
    135
}

#[cfg(target_arch = "aarch64")]
fn personality_syscall_nr() -> i64 {
    92
}

#[cfg(target_arch = "x86_64")]
fn native_mount_syscall_nr() -> Option<i64> {
    Some(165)
}

#[cfg(target_arch = "aarch64")]
fn native_mount_syscall_nr() -> Option<i64> {
    Some(40)
}
