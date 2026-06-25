// Unit condition checking
//
// Implements ConditionPathExists=, ConditionVirtualization=, ConditionCapability=, etc.

use crate::units::Unit;

use super::{Manager, VirtualizationType};

/// Result of evaluating a single condition value after parsing prefixes.
struct ConditionInput<'a> {
    /// Whether the condition is negated (! prefix)
    negated: bool,
    /// Whether this is a trigger condition (| prefix, OR logic)
    trigger: bool,
    /// The actual value to check
    value: &'a str,
}

fn parse_condition(raw: &str) -> ConditionInput<'_> {
    let (trigger, rest) = match raw.strip_prefix('|') {
        Some(r) => (true, r),
        None => (false, raw),
    };
    let (negated, value) = match rest.strip_prefix('!') {
        Some(v) => (true, v),
        None => (false, rest),
    };
    ConditionInput {
        negated,
        trigger,
        value,
    }
}

/// Check a list of conditions where each value is checked against `test_fn`.
/// Regular conditions use AND logic (all must pass).
/// Trigger conditions (| prefix) use OR logic (any must pass).
fn check_condition_list(
    values: &[String],
    condition_name: &str,
    fail_when_true: &str,
    fail_when_false: &str,
    test_fn: impl Fn(&str) -> bool,
) -> Option<String> {
    let mut triggers: Vec<(&str, bool)> = Vec::new();

    for raw in values {
        let input = parse_condition(raw);
        let result = test_fn(input.value);

        if input.trigger {
            triggers.push((input.value, input.negated));
            continue;
        }

        // Regular condition: must pass
        if let Some(msg) = check_negatable(
            input.negated,
            result,
            condition_name,
            input.value,
            fail_when_true,
            fail_when_false,
        ) {
            return Some(msg);
        }
    }

    // Trigger conditions: at least one must pass (OR)
    check_triggers(&triggers, condition_name, &test_fn)
}

fn check_negatable(
    negated: bool,
    result: bool,
    condition_name: &str,
    value: &str,
    fail_when_true: &str,
    fail_when_false: &str,
) -> Option<String> {
    if negated && result {
        Some(format!(
            "{}=!{} failed ({})",
            condition_name, value, fail_when_true
        ))
    } else if !negated && !result {
        Some(format!(
            "{}={} failed ({})",
            condition_name, value, fail_when_false
        ))
    } else {
        None
    }
}

fn check_triggers(
    triggers: &[(&str, bool)],
    condition_name: &str,
    test_fn: &impl Fn(&str) -> bool,
) -> Option<String> {
    if triggers.is_empty() {
        return None;
    }

    let any_pass = triggers.iter().any(|(value, negated)| {
        let result = test_fn(value);
        if *negated {
            !result
        } else {
            result
        }
    });

    if any_pass {
        return None;
    }

    let failed: Vec<_> = triggers
        .iter()
        .map(|(v, n)| {
            if *n {
                format!("|!{v}")
            } else {
                format!("|{v}")
            }
        })
        .collect();
    Some(format!(
        "{}={} failed (no trigger condition matched)",
        condition_name,
        failed.join(", ")
    ))
}

impl Manager {
    /// Check if unit conditions are met.
    /// Returns None if all conditions pass, or Some(reason) if a condition fails.
    pub(super) fn check_conditions(&self, unit: &Unit) -> Option<String> {
        let section = unit.unit_section();
        let virtualization = self.detect_virtualization();
        let matched_virtualization = format!("matched {:?}", virtualization);
        let detected_virtualization = format!("detected {:?}", virtualization);

        let failed_group = [
            check_condition_list(
                &section.condition_path_exists,
                "ConditionPathExists",
                "path exists",
                "path missing",
                |path| std::path::Path::new(path).exists(),
            ),
            check_condition_list(
                &section.condition_directory_not_empty,
                "ConditionDirectoryNotEmpty",
                "not empty",
                "empty or missing",
                is_directory_not_empty,
            ),
            check_condition_list(
                &section.condition_virtualization,
                "ConditionVirtualization",
                &matched_virtualization,
                &detected_virtualization,
                |check| self.check_virtualization_match_with(&virtualization, check),
            ),
            check_condition_list(
                &section.condition_capability,
                "ConditionCapability",
                "capability present",
                "capability missing",
                |cap| self.check_capability(cap),
            ),
            check_condition_list(
                &section.condition_kernel_command_line,
                "ConditionKernelCommandLine",
                "parameter present",
                "parameter missing",
                |param| self.check_kernel_cmdline(param),
            ),
            check_condition_list(
                &section.condition_security,
                "ConditionSecurity",
                "security framework active",
                "security framework not active",
                |framework| self.check_security_framework(framework),
            ),
            check_condition_list(
                &section.condition_needs_update,
                "ConditionNeedsUpdate",
                "update needed",
                "no update needed",
                |path| self.check_needs_update(path, false),
            ),
        ]
        .into_iter()
        .find_map(std::convert::identity);

        failed_group.or_else(|| self.check_first_boot_condition(section.condition_first_boot))
    }

    fn check_virtualization_match_with(
        &self,
        detected: &Option<VirtualizationType>,
        check: &str,
    ) -> bool {
        match check.to_lowercase().as_str() {
            "yes" | "true" => detected.is_some(),
            "no" | "false" => detected.is_none(),
            "vm" => detected.as_ref().map(|v| v.is_vm()).unwrap_or(false),
            "container" => detected.as_ref().map(|v| v.is_container()).unwrap_or(false),
            specific => detected
                .as_ref()
                .map(|v| v.matches(specific))
                .unwrap_or(false),
        }
    }

    fn check_first_boot_condition(&self, first_boot_wanted: Option<bool>) -> Option<String> {
        let first_boot_wanted = first_boot_wanted?;
        let is_first_boot = self.check_first_boot();

        if first_boot_wanted && !is_first_boot {
            return Some("ConditionFirstBoot=yes failed (not first boot)".to_string());
        }
        if !first_boot_wanted && is_first_boot {
            return Some("ConditionFirstBoot=no failed (is first boot)".to_string());
        }
        None
    }

    /// Detected virtualization type
    pub(super) fn detect_virtualization(&self) -> Option<VirtualizationType> {
        detect_container().or_else(detect_vm)
    }

    /// Check if process has a specific capability
    fn check_capability(&self, cap_name: &str) -> bool {
        let Some(cap_num) = capability_number(cap_name) else {
            return false;
        };

        let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
            return false;
        };

        status
            .lines()
            .find_map(|line| line.strip_prefix("CapEff:\t"))
            .and_then(|hex| u64::from_str_radix(hex.trim(), 16).ok())
            .map(|caps| (caps & (1u64 << cap_num)) != 0)
            .unwrap_or(false)
    }

    /// Check if kernel command line contains parameter
    fn check_kernel_cmdline(&self, param: &str) -> bool {
        let Ok(cmdline) = std::fs::read_to_string("/proc/cmdline") else {
            return false;
        };

        if param.contains('=') {
            cmdline.split_whitespace().any(|p| p == param)
        } else {
            cmdline
                .split_whitespace()
                .any(|p| p == param || p.starts_with(&format!("{}=", param)))
        }
    }

    /// Check if security framework is active
    fn check_security_framework(&self, framework: &str) -> bool {
        match framework.to_lowercase().as_str() {
            "selinux" => std::path::Path::new("/sys/fs/selinux").exists(),
            "apparmor" => std::path::Path::new("/sys/kernel/security/apparmor").exists(),
            "smack" => std::path::Path::new("/sys/fs/smackfs").exists(),
            "tomoyo" => std::path::Path::new("/sys/kernel/security/tomoyo").exists(),
            "ima" => std::path::Path::new("/sys/kernel/security/ima").exists(),
            "audit" => std::path::Path::new("/proc/self/loginuid").exists(),
            "uefi-secureboot" => has_efi_var_prefix("SecureBoot-"),
            "tpm2" => std::fs::read_to_string("/sys/class/tpm/tpm0/tpm_version_major")
                .map(|v| v.trim() == "2")
                .unwrap_or(false),
            "measured-uki" => has_efi_var_prefix("StubInfo-4a67b082-0a4c-41cf-b6c7-440b29bb8c4f"),
            "cvm" => is_confidential_vm(),
            _ => false,
        }
    }

    /// Check if this is first boot
    fn check_first_boot(&self) -> bool {
        if std::path::Path::new("/run/systemd/first-boot").exists() {
            return true;
        }

        match std::fs::read_to_string("/etc/machine-id") {
            Ok(id) => {
                let content = id.trim();
                content.is_empty() || content.chars().all(|c| c == '0')
            }
            Err(_) => true,
        }
    }

    /// Check if a directory needs update (for ConditionNeedsUpdate)
    fn check_needs_update(&self, path: &str, trigger: bool) -> bool {
        let (check_path, flag_file) = match path.to_lowercase().as_str() {
            "/etc" => ("/etc", "/var/lib/systemd/update-done.d/etc"),
            "/var" => ("/var", "/var/lib/systemd/update-done.d/var"),
            _ => return false,
        };

        let flag_path = std::path::Path::new(flag_file);
        if !flag_path.exists() {
            return true;
        }

        let flag_mtime = match std::fs::metadata(flag_path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return trigger,
        };
        let dir_mtime = match std::fs::metadata(check_path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return false,
        };

        dir_mtime > flag_mtime
    }
}

fn is_directory_not_empty(path: &str) -> bool {
    let dir = std::path::Path::new(path);
    dir.is_dir()
        && std::fs::read_dir(dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
}

fn detect_container() -> Option<VirtualizationType> {
    detect_container_from_marker_files()
        .or_else(detect_container_from_environ)
        .or_else(detect_container_from_cgroup)
}

fn detect_container_from_marker_files() -> Option<VirtualizationType> {
    if std::path::Path::new("/.dockerenv").exists() {
        return Some(VirtualizationType::Docker);
    }
    if std::path::Path::new("/run/.containerenv").exists() {
        return Some(VirtualizationType::Podman);
    }
    None
}

fn detect_container_from_environ() -> Option<VirtualizationType> {
    let environ = std::fs::read_to_string("/proc/1/environ").ok()?;
    if !environ.contains("container=") {
        return None;
    }

    let value = environ
        .split('\0')
        .find_map(|part| part.strip_prefix("container="));

    Some(
        value
            .map(VirtualizationType::from_container_env)
            .unwrap_or(VirtualizationType::Container),
    )
}

fn detect_container_from_cgroup() -> Option<VirtualizationType> {
    let cgroup = std::fs::read_to_string("/proc/1/cgroup").ok()?;
    if cgroup.contains("/machine.slice/") || cgroup.contains("machine-") {
        return Some(VirtualizationType::SystemdNspawn);
    }
    None
}

fn detect_vm() -> Option<VirtualizationType> {
    if let Some(vt) = detect_vm_from_dmi("/sys/class/dmi/id/product_name") {
        return Some(vt);
    }
    detect_vm_from_dmi("/sys/class/dmi/id/sys_vendor")
}

fn detect_vm_from_dmi(path: &str) -> Option<VirtualizationType> {
    let content = std::fs::read_to_string(path).ok()?;
    let lower = content.trim().to_lowercase();

    let patterns: &[(&str, VirtualizationType)] = &[
        ("virtualbox", VirtualizationType::VirtualBox),
        ("innotek", VirtualizationType::VirtualBox),
        ("oracle", VirtualizationType::VirtualBox),
        ("vmware", VirtualizationType::VMware),
        ("qemu", VirtualizationType::Qemu),
        ("kvm", VirtualizationType::Qemu),
        ("bochs", VirtualizationType::Bochs),
        ("xen", VirtualizationType::Xen),
        ("hyper-v", VirtualizationType::HyperV),
        ("microsoft", VirtualizationType::HyperV),
    ];

    patterns
        .iter()
        .find(|(needle, _)| lower.contains(needle))
        .map(|(_, vt)| vt.clone())
}

fn has_efi_var_prefix(prefix: &str) -> bool {
    std::fs::read_dir("/sys/firmware/efi/efivars")
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|e| e.file_name().to_string_lossy().starts_with(prefix))
        })
        .unwrap_or(false)
}

fn is_confidential_vm() -> bool {
    std::path::Path::new("/sys/kernel/security/coco/tdx_guest").exists()
        || std::path::Path::new("/sys/kernel/security/coco/sev_guest").exists()
        || std::fs::read_to_string("/sys/devices/system/cpu/vulnerabilities/sev")
            .map(|v| v.contains("SEV"))
            .unwrap_or(false)
}

const CAPABILITY_NUMBERS: [(&str, u32); 41] = [
    ("CAP_CHOWN", 0),
    ("CAP_DAC_OVERRIDE", 1),
    ("CAP_DAC_READ_SEARCH", 2),
    ("CAP_FOWNER", 3),
    ("CAP_FSETID", 4),
    ("CAP_KILL", 5),
    ("CAP_SETGID", 6),
    ("CAP_SETUID", 7),
    ("CAP_SETPCAP", 8),
    ("CAP_LINUX_IMMUTABLE", 9),
    ("CAP_NET_BIND_SERVICE", 10),
    ("CAP_NET_BROADCAST", 11),
    ("CAP_NET_ADMIN", 12),
    ("CAP_NET_RAW", 13),
    ("CAP_IPC_LOCK", 14),
    ("CAP_IPC_OWNER", 15),
    ("CAP_SYS_MODULE", 16),
    ("CAP_SYS_RAWIO", 17),
    ("CAP_SYS_CHROOT", 18),
    ("CAP_SYS_PTRACE", 19),
    ("CAP_SYS_PACCT", 20),
    ("CAP_SYS_ADMIN", 21),
    ("CAP_SYS_BOOT", 22),
    ("CAP_SYS_NICE", 23),
    ("CAP_SYS_RESOURCE", 24),
    ("CAP_SYS_TIME", 25),
    ("CAP_SYS_TTY_CONFIG", 26),
    ("CAP_MKNOD", 27),
    ("CAP_LEASE", 28),
    ("CAP_AUDIT_WRITE", 29),
    ("CAP_AUDIT_CONTROL", 30),
    ("CAP_SETFCAP", 31),
    ("CAP_MAC_OVERRIDE", 32),
    ("CAP_MAC_ADMIN", 33),
    ("CAP_SYSLOG", 34),
    ("CAP_WAKE_ALARM", 35),
    ("CAP_BLOCK_SUSPEND", 36),
    ("CAP_AUDIT_READ", 37),
    ("CAP_PERFMON", 38),
    ("CAP_BPF", 39),
    ("CAP_CHECKPOINT_RESTORE", 40),
];

fn capability_number(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    CAPABILITY_NUMBERS
        .iter()
        .find(|(cap_name, _)| *cap_name == upper.as_str())
        .map(|(_, number)| *number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Service;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEMP_ID: AtomicUsize = AtomicUsize::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(label: &str) -> TempDir {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("sysd-conditions-{label}-{id}"));
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn service_unit(configure: impl FnOnce(&mut Service)) -> Unit {
        let mut service = Service::new("demo.service".to_string());
        configure(&mut service);
        Unit::Service(service)
    }

    #[test]
    fn parse_condition_handles_trigger_and_negation_prefixes() {
        let plain = parse_condition("/tmp/demo");
        assert!(!plain.trigger);
        assert!(!plain.negated);
        assert_eq!(plain.value, "/tmp/demo");

        let negated = parse_condition("!/tmp/demo");
        assert!(!negated.trigger);
        assert!(negated.negated);
        assert_eq!(negated.value, "/tmp/demo");

        let trigger = parse_condition("|!/tmp/demo");
        assert!(trigger.trigger);
        assert!(trigger.negated);
        assert_eq!(trigger.value, "/tmp/demo");
    }

    #[test]
    fn condition_list_requires_regular_conditions_and_accepts_any_trigger() {
        let matches_present = |value: &str| value == "present";

        assert_eq!(
            check_condition_list(
                &["present".to_string()],
                "ConditionDemo",
                "was present",
                "was missing",
                matches_present,
            ),
            None
        );

        let regular_failure = check_condition_list(
            &["missing".to_string()],
            "ConditionDemo",
            "was present",
            "was missing",
            matches_present,
        )
        .unwrap();
        assert!(regular_failure.contains("ConditionDemo=missing"));
        assert!(regular_failure.contains("was missing"));

        let negated_failure = check_condition_list(
            &["!present".to_string()],
            "ConditionDemo",
            "was present",
            "was missing",
            matches_present,
        )
        .unwrap();
        assert!(negated_failure.contains("ConditionDemo=!present"));
        assert!(negated_failure.contains("was present"));

        assert_eq!(
            check_condition_list(
                &["|missing".to_string(), "|present".to_string()],
                "ConditionDemo",
                "was present",
                "was missing",
                matches_present,
            ),
            None
        );

        let trigger_failure = check_condition_list(
            &["|missing".to_string(), "|absent".to_string()],
            "ConditionDemo",
            "was present",
            "was missing",
            matches_present,
        )
        .unwrap();
        assert!(trigger_failure.contains("|missing, |absent"));
        assert!(trigger_failure.contains("no trigger condition matched"));
    }

    #[test]
    fn directory_not_empty_reports_only_non_empty_directories() {
        let root = temp_dir("directory-not-empty");
        let empty_dir = root.path().join("empty");
        let full_dir = root.path().join("full");
        let file_path = root.path().join("file");
        std::fs::create_dir(&empty_dir).unwrap();
        std::fs::create_dir(&full_dir).unwrap();
        std::fs::write(full_dir.join("entry"), "data").unwrap();
        std::fs::write(&file_path, "data").unwrap();

        assert!(!is_directory_not_empty(empty_dir.to_str().unwrap()));
        assert!(is_directory_not_empty(full_dir.to_str().unwrap()));
        assert!(!is_directory_not_empty(file_path.to_str().unwrap()));
        assert!(!is_directory_not_empty(
            root.path().join("missing").to_str().unwrap()
        ));
    }

    #[test]
    fn capability_number_is_case_insensitive_and_rejects_unknown_names() {
        assert_eq!(capability_number("cap_chown"), Some(0));
        assert_eq!(capability_number("CAP_CHECKPOINT_RESTORE"), Some(40));
        assert_eq!(capability_number("CAP_DOES_NOT_EXIST"), None);
    }

    #[test]
    fn detect_vm_from_dmi_matches_known_product_markers() {
        let root = temp_dir("dmi");
        let product = root.path().join("product_name");

        std::fs::write(&product, "VMware Virtual Platform").unwrap();
        assert_eq!(
            detect_vm_from_dmi(product.to_str().unwrap()),
            Some(VirtualizationType::VMware)
        );

        std::fs::write(&product, "Microsoft Corporation Hyper-V").unwrap();
        assert_eq!(
            detect_vm_from_dmi(product.to_str().unwrap()),
            Some(VirtualizationType::HyperV)
        );

        std::fs::write(&product, "KVM").unwrap();
        assert_eq!(
            detect_vm_from_dmi(product.to_str().unwrap()),
            Some(VirtualizationType::Qemu)
        );

        std::fs::write(&product, "bare metal").unwrap();
        assert_eq!(detect_vm_from_dmi(product.to_str().unwrap()), None);
        assert_eq!(
            detect_vm_from_dmi(root.path().join("missing").to_str().unwrap()),
            None
        );
    }

    #[test]
    fn virtualization_match_handles_generic_specific_and_negative_checks() {
        let manager = Manager::new();
        let docker = Some(VirtualizationType::Docker);
        let qemu = Some(VirtualizationType::Qemu);
        let none = None;

        assert!(manager.check_virtualization_match_with(&docker, "yes"));
        assert!(manager.check_virtualization_match_with(&none, "no"));
        assert!(manager.check_virtualization_match_with(&docker, "container"));
        assert!(!manager.check_virtualization_match_with(&docker, "vm"));
        assert!(manager.check_virtualization_match_with(&qemu, "vm"));
        assert!(manager.check_virtualization_match_with(&qemu, "kvm"));
        assert!(!manager.check_virtualization_match_with(&none, "container"));
    }

    #[test]
    fn check_conditions_handles_path_and_directory_conditions() {
        let manager = Manager::new();
        let root = temp_dir("manager");
        let existing_path = root.path().join("exists");
        let missing_path = root.path().join("missing");
        let full_dir = root.path().join("full");
        let empty_dir = root.path().join("empty");
        std::fs::write(&existing_path, "data").unwrap();
        std::fs::create_dir(&full_dir).unwrap();
        std::fs::write(full_dir.join("entry"), "data").unwrap();
        std::fs::create_dir(&empty_dir).unwrap();

        let passing = service_unit(|service| {
            service.unit.condition_path_exists =
                vec![existing_path.to_string_lossy().to_string()];
            service.unit.condition_directory_not_empty =
                vec![full_dir.to_string_lossy().to_string()];
        });
        assert_eq!(manager.check_conditions(&passing), None);

        let missing = service_unit(|service| {
            service.unit.condition_path_exists = vec![missing_path.to_string_lossy().to_string()];
        });
        let missing_failure = manager.check_conditions(&missing).unwrap();
        assert!(missing_failure.contains("ConditionPathExists="));
        assert!(missing_failure.contains("path missing"));

        let empty = service_unit(|service| {
            service.unit.condition_directory_not_empty =
                vec![empty_dir.to_string_lossy().to_string()];
        });
        let empty_failure = manager.check_conditions(&empty).unwrap();
        assert!(empty_failure.contains("ConditionDirectoryNotEmpty="));
        assert!(empty_failure.contains("empty or missing"));
    }

    #[test]
    fn check_conditions_respects_negated_path_conditions() {
        let manager = Manager::new();
        let root = temp_dir("negated-path");
        let existing_path = root.path().join("exists");
        let missing_path = root.path().join("missing");
        std::fs::write(&existing_path, "data").unwrap();

        let passing = service_unit(|service| {
            service.unit.condition_path_exists =
                vec![format!("!{}", missing_path.to_string_lossy())];
        });
        assert_eq!(manager.check_conditions(&passing), None);

        let failing = service_unit(|service| {
            service.unit.condition_path_exists =
                vec![format!("!{}", existing_path.to_string_lossy())];
        });
        let failure = manager.check_conditions(&failing).unwrap();
        assert!(failure.contains("ConditionPathExists=!"));
        assert!(failure.contains("path exists"));
    }
}
