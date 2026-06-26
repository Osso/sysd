use super::*;
use crate::units::{Service, Unit};

fn service_unit(configure: impl FnOnce(&mut Service)) -> Unit {
    let mut service = Service::new("demo.service".to_string());
    configure(&mut service);
    Unit::Service(service)
}

#[test]
fn condition_list_reports_negated_trigger_failures() {
    let matches_present = |value: &str| value == "present";

    let failure = check_condition_list(
        &["|!present".to_string(), "|missing".to_string()],
        "ConditionDemo",
        "was present",
        "was missing",
        matches_present,
    )
    .unwrap();

    assert!(failure.contains("|!present, |missing"));
}

#[test]
fn check_conditions_reports_capability_kernel_security_and_needs_update_failures() {
    let manager = Manager::new();

    let missing_capability = service_unit(|service| {
        service.unit.condition_capability = vec!["CAP_DOES_NOT_EXIST".to_string()];
    });
    assert!(manager
        .check_conditions(&missing_capability)
        .unwrap()
        .contains("ConditionCapability=CAP_DOES_NOT_EXIST"));

    let missing_kernel_param = service_unit(|service| {
        service.unit.condition_kernel_command_line =
            vec!["definitely_missing_sysd_param".to_string()];
    });
    assert!(manager
        .check_conditions(&missing_kernel_param)
        .unwrap()
        .contains("ConditionKernelCommandLine=definitely_missing_sysd_param"));

    let missing_security = service_unit(|service| {
        service.unit.condition_security = vec!["definitely-missing-framework".to_string()];
    });
    assert!(manager
        .check_conditions(&missing_security)
        .unwrap()
        .contains("ConditionSecurity=definitely-missing-framework"));

    let missing_update_path = service_unit(|service| {
        service.unit.condition_needs_update = vec!["/definitely-missing".to_string()];
    });
    assert!(manager
        .check_conditions(&missing_update_path)
        .unwrap()
        .contains("ConditionNeedsUpdate=/definitely-missing"));
}

#[test]
fn first_boot_condition_reports_opposite_of_detected_state() {
    let manager = Manager::new();
    let detected_first_boot = manager.check_first_boot();

    let unit = service_unit(|service| {
        service.unit.condition_first_boot = Some(!detected_first_boot);
    });
    let failure = manager.check_conditions(&unit).unwrap();

    if detected_first_boot {
        assert!(failure.contains("ConditionFirstBoot=no failed"));
    } else {
        assert!(failure.contains("ConditionFirstBoot=yes failed"));
    }
}

#[test]
fn capability_checks_match_current_process_status() {
    let manager = Manager::new();
    let expected = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("CapEff:\t"))
                .and_then(|hex| u64::from_str_radix(hex.trim(), 16).ok())
        })
        .map(|caps| (caps & 1) != 0)
        .unwrap_or(false);

    assert_eq!(manager.check_capability("CAP_CHOWN"), expected);
    assert!(!manager.check_capability("CAP_NOT_REAL"));
}

#[test]
fn security_framework_checks_follow_kernel_probe_files() {
    let manager = Manager::new();

    assert_eq!(
        manager.check_security_framework("audit"),
        std::path::Path::new("/proc/self/loginuid").exists()
    );
    assert_eq!(
        manager.check_security_framework("tpm2"),
        std::fs::read_to_string("/sys/class/tpm/tpm0/tpm_version_major")
            .map(|v| v.trim() == "2")
            .unwrap_or(false)
    );
    assert_eq!(
        manager.check_security_framework("uefi-secureboot"),
        has_efi_var_prefix("SecureBoot-")
    );
    assert_eq!(
        manager.check_security_framework("measured-uki"),
        has_efi_var_prefix("StubInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f")
    );
    assert_eq!(manager.check_security_framework("cvm"), is_confidential_vm());
}

#[test]
fn needs_update_matches_update_flag_timestamps() {
    let manager = Manager::new();

    assert_eq!(
        manager.check_needs_update("/etc", false),
        expected_needs_update("/etc", "/var/lib/systemd/update-done.d/etc", false)
    );
    assert_eq!(
        manager.check_needs_update("/var", true),
        expected_needs_update("/var", "/var/lib/systemd/update-done.d/var", true)
    );
}

fn expected_needs_update(path: &str, flag_file: &str, trigger: bool) -> bool {
    let flag_path = std::path::Path::new(flag_file);
    if !flag_path.exists() {
        return true;
    }

    let flag_mtime = match std::fs::metadata(flag_path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return trigger,
    };
    let dir_mtime = match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return false,
    };

    dir_mtime > flag_mtime
}
