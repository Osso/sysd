//! Unit condition checking
//!
//! Implements ConditionPathExists=, ConditionVirtualization=, ConditionCapability=, etc.

use crate::units::Unit;

use super::{Manager, VirtualizationType};

impl Manager {
    /// Check if unit conditions are met
    /// Returns None if all conditions pass, or Some(reason) if a condition fails
    pub(super) fn check_conditions(&self, unit: &Unit) -> Option<String> {
        let section = unit.unit_section();

        // ConditionPathExists - path must exist (or not exist if prefixed with !)
        for path in &section.condition_path_exists {
            let (negated, path) = if let Some(p) = path.strip_prefix('!') {
                (true, p)
            } else {
                (false, path.as_str())
            };

            let exists = std::path::Path::new(path).exists();
            if negated && exists {
                return Some(format!(
                    "ConditionPathExists=!{} failed (path exists)",
                    path
                ));
            }
            if !negated && !exists {
                return Some(format!(
                    "ConditionPathExists={} failed (path missing)",
                    path
                ));
            }
        }

        // ConditionDirectoryNotEmpty - directory must exist and have entries
        for path in &section.condition_directory_not_empty {
            let (negated, path) = if let Some(p) = path.strip_prefix('!') {
                (true, p)
            } else {
                (false, path.as_str())
            };

            let dir_path = std::path::Path::new(path);
            let is_not_empty = dir_path.is_dir()
                && std::fs::read_dir(dir_path)
                    .map(|mut d| d.next().is_some())
                    .unwrap_or(false);

            if negated && is_not_empty {
                return Some(format!(
                    "ConditionDirectoryNotEmpty=!{} failed (not empty)",
                    path
                ));
            }
            if !negated && !is_not_empty {
                return Some(format!(
                    "ConditionDirectoryNotEmpty={} failed (empty or missing)",
                    path
                ));
            }
        }

        // ConditionVirtualization - check if running in VM/container
        for virt in &section.condition_virtualization {
            let (negated, check) = if let Some(v) = virt.strip_prefix('!') {
                (true, v)
            } else {
                (false, virt.as_str())
            };

            let detected = self.detect_virtualization();
            let matches = match check.to_lowercase().as_str() {
                "yes" | "true" => detected.is_some(),
                "no" | "false" => detected.is_none(),
                "vm" => detected.as_ref().map(|v| v.is_vm()).unwrap_or(false),
                "container" => detected.as_ref().map(|v| v.is_container()).unwrap_or(false),
                specific => detected.as_ref().map(|v| v.matches(specific)).unwrap_or(false),
            };

            if negated && matches {
                return Some(format!(
                    "ConditionVirtualization=!{} failed (matched {:?})",
                    check, detected
                ));
            }
            if !negated && !matches {
                return Some(format!(
                    "ConditionVirtualization={} failed (detected {:?})",
                    check, detected
                ));
            }
        }

        // ConditionCapability - check if process has capability
        for cap in &section.condition_capability {
            let (negated, cap_name) = if let Some(c) = cap.strip_prefix('!') {
                (true, c)
            } else {
                (false, cap.as_str())
            };

            let has_cap = self.check_capability(cap_name);

            if negated && has_cap {
                return Some(format!(
                    "ConditionCapability=!{} failed (capability present)",
                    cap_name
                ));
            }
            if !negated && !has_cap {
                return Some(format!(
                    "ConditionCapability={} failed (capability missing)",
                    cap_name
                ));
            }
        }

        // ConditionKernelCommandLine - check /proc/cmdline
        for param in &section.condition_kernel_command_line {
            let (negated, check) = if let Some(p) = param.strip_prefix('!') {
                (true, p)
            } else {
                (false, param.as_str())
            };

            let has_param = self.check_kernel_cmdline(check);

            if negated && has_param {
                return Some(format!(
                    "ConditionKernelCommandLine=!{} failed (parameter present)",
                    check
                ));
            }
            if !negated && !has_param {
                return Some(format!(
                    "ConditionKernelCommandLine={} failed (parameter missing)",
                    check
                ));
            }
        }

        // ConditionSecurity - check security framework
        for sec in &section.condition_security {
            let (negated, framework) = if let Some(s) = sec.strip_prefix('!') {
                (true, s)
            } else {
                (false, sec.as_str())
            };

            let has_framework = self.check_security_framework(framework);

            if negated && has_framework {
                return Some(format!(
                    "ConditionSecurity=!{} failed (security framework active)",
                    framework
                ));
            }
            if !negated && !has_framework {
                return Some(format!(
                    "ConditionSecurity={} failed (security framework not active)",
                    framework
                ));
            }
        }

        // ConditionFirstBoot - check if this is first boot
        if let Some(first_boot_wanted) = section.condition_first_boot {
            let is_first_boot = self.check_first_boot();
            if first_boot_wanted && !is_first_boot {
                return Some("ConditionFirstBoot=yes failed (not first boot)".to_string());
            }
            if !first_boot_wanted && is_first_boot {
                return Some("ConditionFirstBoot=no failed (is first boot)".to_string());
            }
        }

        // ConditionNeedsUpdate - check if /etc or /var needs update
        for update in &section.condition_needs_update {
            let (negated, check) = if let Some(u) = update.strip_prefix('!') {
                (true, u)
            } else {
                (false, update.as_str())
            };

            // Handle trigger prefix (|) - passes on first boot even if no update needed
            let (trigger, path) = if let Some(p) = check.strip_prefix('|') {
                (true, p)
            } else {
                (false, check)
            };

            let needs_update = self.check_needs_update(path, trigger);

            if negated && needs_update {
                return Some(format!(
                    "ConditionNeedsUpdate=!{} failed (update needed)",
                    check
                ));
            }
            if !negated && !needs_update {
                return Some(format!(
                    "ConditionNeedsUpdate={} failed (no update needed)",
                    check
                ));
            }
        }

        None
    }

    /// Detected virtualization type
    pub(super) fn detect_virtualization(&self) -> Option<VirtualizationType> {
        // Check for container environments
        if std::path::Path::new("/.dockerenv").exists() {
            return Some(VirtualizationType::Docker);
        }
        if std::path::Path::new("/run/.containerenv").exists() {
            return Some(VirtualizationType::Podman);
        }

        // Check /proc/1/environ for container markers
        if let Ok(environ) = std::fs::read_to_string("/proc/1/environ") {
            if environ.contains("container=") {
                // Parse the container type
                for part in environ.split('\0') {
                    if let Some(val) = part.strip_prefix("container=") {
                        return Some(VirtualizationType::from_container_env(val));
                    }
                }
                return Some(VirtualizationType::Container);
            }
        }

        // Check /proc/1/cgroup for systemd-nspawn
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            if cgroup.contains("/machine.slice/") || cgroup.contains("machine-") {
                return Some(VirtualizationType::SystemdNspawn);
            }
        }

        // Check for VM via DMI (requires /sys/class/dmi/id/)
        if let Ok(product) = std::fs::read_to_string("/sys/class/dmi/id/product_name") {
            let product = product.trim().to_lowercase();
            if product.contains("virtualbox") {
                return Some(VirtualizationType::VirtualBox);
            }
            if product.contains("vmware") {
                return Some(VirtualizationType::VMware);
            }
            if product.contains("qemu") || product.contains("kvm") {
                return Some(VirtualizationType::Qemu);
            }
            if product.contains("bochs") {
                return Some(VirtualizationType::Bochs);
            }
            if product.contains("xen") {
                return Some(VirtualizationType::Xen);
            }
            if product.contains("hyper-v") || product.contains("microsoft") {
                return Some(VirtualizationType::HyperV);
            }
        }

        // Check for VM via sys_vendor
        if let Ok(vendor) = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor") {
            let vendor = vendor.trim().to_lowercase();
            if vendor.contains("qemu") {
                return Some(VirtualizationType::Qemu);
            }
            if vendor.contains("vmware") {
                return Some(VirtualizationType::VMware);
            }
            if vendor.contains("innotek") || vendor.contains("oracle") {
                return Some(VirtualizationType::VirtualBox);
            }
        }

        None
    }

    /// Check if process has a specific capability
    fn check_capability(&self, cap_name: &str) -> bool {
        // Map capability name to number
        let cap_num = match cap_name.to_uppercase().as_str() {
            "CAP_CHOWN" => 0,
            "CAP_DAC_OVERRIDE" => 1,
            "CAP_DAC_READ_SEARCH" => 2,
            "CAP_FOWNER" => 3,
            "CAP_FSETID" => 4,
            "CAP_KILL" => 5,
            "CAP_SETGID" => 6,
            "CAP_SETUID" => 7,
            "CAP_SETPCAP" => 8,
            "CAP_LINUX_IMMUTABLE" => 9,
            "CAP_NET_BIND_SERVICE" => 10,
            "CAP_NET_BROADCAST" => 11,
            "CAP_NET_ADMIN" => 12,
            "CAP_NET_RAW" => 13,
            "CAP_IPC_LOCK" => 14,
            "CAP_IPC_OWNER" => 15,
            "CAP_SYS_MODULE" => 16,
            "CAP_SYS_RAWIO" => 17,
            "CAP_SYS_CHROOT" => 18,
            "CAP_SYS_PTRACE" => 19,
            "CAP_SYS_PACCT" => 20,
            "CAP_SYS_ADMIN" => 21,
            "CAP_SYS_BOOT" => 22,
            "CAP_SYS_NICE" => 23,
            "CAP_SYS_RESOURCE" => 24,
            "CAP_SYS_TIME" => 25,
            "CAP_SYS_TTY_CONFIG" => 26,
            "CAP_MKNOD" => 27,
            "CAP_LEASE" => 28,
            "CAP_AUDIT_WRITE" => 29,
            "CAP_AUDIT_CONTROL" => 30,
            "CAP_SETFCAP" => 31,
            "CAP_MAC_OVERRIDE" => 32,
            "CAP_MAC_ADMIN" => 33,
            "CAP_SYSLOG" => 34,
            "CAP_WAKE_ALARM" => 35,
            "CAP_BLOCK_SUSPEND" => 36,
            "CAP_AUDIT_READ" => 37,
            "CAP_PERFMON" => 38,
            "CAP_BPF" => 39,
            "CAP_CHECKPOINT_RESTORE" => 40,
            _ => return false, // Unknown capability
        };

        // Read effective capabilities from /proc/self/status
        if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(hex) = line.strip_prefix("CapEff:\t") {
                    if let Ok(caps) = u64::from_str_radix(hex.trim(), 16) {
                        return (caps & (1u64 << cap_num)) != 0;
                    }
                }
            }
        }

        false
    }

    /// Check if kernel command line contains parameter
    fn check_kernel_cmdline(&self, param: &str) -> bool {
        if let Ok(cmdline) = std::fs::read_to_string("/proc/cmdline") {
            // Check for key=value or just key
            if param.contains('=') {
                // Exact match for key=value
                cmdline.split_whitespace().any(|p| p == param)
            } else {
                // Match key or key=anything
                cmdline
                    .split_whitespace()
                    .any(|p| p == param || p.starts_with(&format!("{}=", param)))
            }
        } else {
            false
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
            "uefi-secureboot" => {
                // Check if Secure Boot is enabled
                std::path::Path::new("/sys/firmware/efi/efivars/SecureBoot-*").exists()
                    || std::fs::read_dir("/sys/firmware/efi/efivars")
                        .map(|entries| {
                            entries.filter_map(|e| e.ok()).any(|e| {
                                e.file_name().to_string_lossy().starts_with("SecureBoot-")
                            })
                        })
                        .unwrap_or(false)
            }
            _ => false,
        }
    }

    /// Check if this is first boot
    fn check_first_boot(&self) -> bool {
        // systemd uses /run/systemd/first-boot as marker
        if std::path::Path::new("/run/systemd/first-boot").exists() {
            return true;
        }

        // Also check if /etc/machine-id is empty or uninitialized
        if let Ok(machine_id) = std::fs::read_to_string("/etc/machine-id") {
            let content = machine_id.trim();
            // Uninitialized machine-id is empty or all zeros
            if content.is_empty() || content.chars().all(|c| c == '0') {
                return true;
            }
        } else {
            // If machine-id doesn't exist, it's first boot
            return true;
        }

        false
    }

    /// Check if a directory needs update (for ConditionNeedsUpdate)
    /// Returns true if the directory mtime is newer than the update-done flag file
    fn check_needs_update(&self, path: &str, trigger: bool) -> bool {
        // Determine which directory to check
        let (check_path, flag_file) = match path.to_lowercase().as_str() {
            "/etc" => ("/etc", "/var/lib/systemd/update-done.d/etc"),
            "/var" => ("/var", "/var/lib/systemd/update-done.d/var"),
            _ => return false, // Unknown path
        };

        let flag_path = std::path::Path::new(flag_file);
        let dir_path = std::path::Path::new(check_path);

        // If flag file doesn't exist
        if !flag_path.exists() {
            // In trigger mode, this means we need to run
            // In non-trigger mode, this also means update needed
            return true;
        }

        // Compare mtimes
        let flag_mtime = match std::fs::metadata(flag_path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return trigger, // Can't read flag, trigger mode determines result
        };

        let dir_mtime = match std::fs::metadata(dir_path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return false, // Can't read dir, assume no update needed
        };

        // Directory is newer than flag file = update needed
        dir_mtime > flag_mtime
    }
}
