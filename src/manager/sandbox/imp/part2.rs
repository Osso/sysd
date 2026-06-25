fn apply_path_restrictions(
    read_write: &[std::path::PathBuf],
    read_only: &[std::path::PathBuf],
    inaccessible: &[std::path::PathBuf],
) -> Result<(), String> {
    // Make paths inaccessible
    for path in inaccessible {
        if path.exists() {
            make_inaccessible(path.to_str().unwrap_or(""))?;
        }
    }

    // Make paths read-only
    for path in read_only {
        if path.exists() {
            bind_mount_ro(path.to_str().unwrap_or(""))?;
        }
    }

    // Read-write paths are allowed by default, but if we're in strict mode
    // we need to explicitly remount them as writable
    for path in read_write {
        if path.exists() {
            let _ = remount_rw(path.to_str().unwrap_or(""));
        }
    }

    Ok(())
}

/// Add seccomp rules to block namespace creation based on RestrictNamespaces
fn add_restrict_namespaces_rules(
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
    blocked_ns: &[String],
) -> Result<(), String> {
    let ns_flags = [
        ("cgroup", libc::CLONE_NEWCGROUP as u64),
        ("ipc", libc::CLONE_NEWIPC as u64),
        ("net", libc::CLONE_NEWNET as u64),
        ("mnt", libc::CLONE_NEWNS as u64),
        ("pid", libc::CLONE_NEWPID as u64),
        ("user", libc::CLONE_NEWUSER as u64),
        ("uts", libc::CLONE_NEWUTS as u64),
    ];
    let blocked = blocked_namespace_flags(blocked_ns, &ns_flags);
    let unshare_nr = libc::SYS_unshare as i64;
    let clone_nr = libc::SYS_clone as i64;
    #[cfg(target_arch = "x86_64")]
    let clone3_nr = 435i64;
    #[cfg(target_arch = "aarch64")]
    let clone3_nr = 435i64;

    for flag in &blocked {
        add_masked_namespace_rule(rules, unshare_nr, *flag)?;
        add_masked_namespace_rule(rules, clone_nr, *flag)?;
        add_unconditional_rule(rules, clone3_nr)?;
    }

    log::debug!("RestrictNamespaces: blocking {:?}", blocked_ns);
    Ok(())
}

fn blocked_namespace_flags(blocked_ns: &[String], ns_flags: &[(&str, u64)]) -> Vec<u64> {
    if blocked_ns.is_empty() {
        return ns_flags.iter().map(|(_, flag)| *flag).collect();
    }
    blocked_ns
        .iter()
        .filter_map(|name| {
            ns_flags
                .iter()
                .find(|(ns_name, _)| ns_name.eq_ignore_ascii_case(name))
                .map(|(_, flag)| *flag)
        })
        .collect()
}

fn add_masked_namespace_rule(
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
    syscall_nr: i64,
    flag: u64,
) -> Result<(), String> {
    let condition = SeccompCondition::new(0, SeccompCmpArgLen::Qword, SeccompCmpOp::MaskedEq(flag), flag)
        .map_err(|e| e.to_string())?;
    let rule = SeccompRule::new(vec![condition]).map_err(|e| e.to_string())?;
    rules.entry(syscall_nr).or_default().push(rule);
    Ok(())
}

/// Add seccomp rules for SystemCallFilter
fn add_syscall_filter_rules(
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
    filters: &[String],
) -> Result<(), String> {
    for filter in filters {
        apply_syscall_filter(rules, filter)?;
    }

    log::debug!("SystemCallFilter: {} rules", filters.len());
    Ok(())
}

const SYSCALL_GROUP_OBSOLETE: &[&str] =
    &["uselib", "create_module", "get_kernel_syms", "query_module"];
const SYSCALL_GROUP_PRIVILEGED: &[&str] = &[
    "acct",
    "bpf",
    "clock_adjtime",
    "clock_settime",
    "delete_module",
    "finit_module",
    "init_module",
    "ioperm",
    "iopl",
    "kexec_file_load",
    "kexec_load",
    "mount",
    "move_mount",
    "open_tree",
    "pivot_root",
    "reboot",
    "setdomainname",
    "sethostname",
    "settimeofday",
    "swapoff",
    "swapon",
    "umount",
    "umount2",
    "vhangup",
];
const SYSCALL_GROUP_RAW_IO: &[&str] = &["ioperm", "iopl", "pciconfig_read", "pciconfig_write"];
const SYSCALL_GROUP_REBOOT: &[&str] = &["reboot", "kexec_load", "kexec_file_load"];
const SYSCALL_GROUP_SWAP: &[&str] = &["swapon", "swapoff"];
const SYSCALL_GROUP_MODULE: &[&str] = &["init_module", "finit_module", "delete_module"];
const SYSCALL_GROUP_MOUNT: &[&str] = &["mount", "umount", "umount2", "pivot_root", "move_mount"];
const SYSCALL_GROUP_CLOCK: &[&str] = &["clock_settime", "clock_adjtime", "settimeofday"];

const SYSCALL_GROUPS: &[(&str, &[&str])] = &[
    ("obsolete", SYSCALL_GROUP_OBSOLETE),
    ("privileged", SYSCALL_GROUP_PRIVILEGED),
    ("raw-io", SYSCALL_GROUP_RAW_IO),
    ("reboot", SYSCALL_GROUP_REBOOT),
    ("swap", SYSCALL_GROUP_SWAP),
    ("module", SYSCALL_GROUP_MODULE),
    ("mount", SYSCALL_GROUP_MOUNT),
    ("clock", SYSCALL_GROUP_CLOCK),
];

fn apply_syscall_filter(
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
    filter: &str,
) -> Result<(), String> {
    let (is_deny, name) = filter
        .strip_prefix('~')
        .map_or((false, filter), |stripped| (true, stripped));
    if !is_deny {
        return Ok(());
    }
    if let Some(group_name) = name.strip_prefix('@') {
        for syscall in get_syscall_group(group_name) {
            if let Some(nr) = syscall_name_to_nr(syscall) {
                add_unconditional_rule(rules, nr)?;
            }
        }
        return Ok(());
    }
    if let Some(nr) = syscall_name_to_nr(name) {
        add_unconditional_rule(rules, nr)?;
    }
    Ok(())
}

/// Get syscalls for a group name
fn get_syscall_group(group: &str) -> &'static [&'static str] {
    if let Some((_, syscalls)) = SYSCALL_GROUPS
        .iter()
        .find(|(group_name, _)| *group_name == group)
    {
        return syscalls;
    }
    log::warn!("Unknown syscall group @{}", group);
    &[]
}

/// Apply combined seccomp filter with M16 extensions
fn apply_combined_seccomp_m16(service: &ServiceSection) -> Result<(), String> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    let errno = service
        .system_call_error_number
        .map_or(libc::EPERM as u32, |n| n as u32);
    collect_combined_seccomp_rules(service, &mut rules)?;
    apply_seccomp_rules(service, rules, errno)?;
    Ok(())
}

fn collect_combined_seccomp_rules(
    service: &ServiceSection,
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
) -> Result<(), String> {
    if let Some(blocked_ns) = service.restrict_namespaces.as_deref() {
        add_restrict_namespaces_rules(rules, blocked_ns)?;
    }
    if !service.system_call_filter.is_empty() {
        add_syscall_filter_rules(rules, &service.system_call_filter)?;
    }
    if service.restrict_realtime {
        add_restrict_realtime_rules(rules)?;
    }
    if service.protect_clock {
        add_protect_clock_rules(rules)?;
    }
    if service.protect_hostname {
        add_protect_hostname_rules(rules)?;
    }
    if service.lock_personality {
        add_lock_personality_rules(rules)?;
    }
    if service.restrict_suid_sgid {
        add_restrict_suid_sgid_rules(rules)?;
    }
    if let Some(families) = &service.restrict_address_families {
        add_restrict_address_families_rules(rules, families)?;
    }
    Ok(())
}

fn apply_seccomp_rules(
    service: &ServiceSection,
    rules: BTreeMap<i64, Vec<SeccompRule>>,
    errno: u32,
) -> Result<(), String> {
    if rules.is_empty() {
        return Ok(());
    }

    let Some(arch) = native_seccomp_arch() else {
        log::warn!("Seccomp: unsupported architecture, skipping filter");
        return Ok(());
    };
    if !service.system_call_architectures.is_empty() {
        log::debug!(
            "SystemCallArchitectures: {:?} (only native enforced)",
            service.system_call_architectures
        );
    }

    let filter = SeccompFilter::new(rules, SeccompAction::Allow, SeccompAction::Errno(errno), arch)
        .map_err(|e| format!("Failed to create seccomp filter: {}", e))?;
    let bpf_prog: BpfProgram = filter
        .try_into()
        .map_err(|e| format!("Failed to compile seccomp filter: {}", e))?;
    seccompiler::apply_filter(&bpf_prog)
        .map_err(|e| format!("Failed to apply seccomp filter: {}", e))?;
    log::debug!("Seccomp filter applied successfully (errno={})", errno);
    Ok(())
}

fn native_seccomp_arch() -> Option<TargetArch> {
    if cfg!(target_arch = "x86_64") {
        return Some(TargetArch::x86_64);
    }
    if cfg!(target_arch = "aarch64") {
        return Some(TargetArch::aarch64);
    }
    None
}

/// Add seccomp rules for RestrictRealtime
fn add_restrict_realtime_rules(rules: &mut BTreeMap<i64, Vec<SeccompRule>>) -> Result<(), String> {
    // Block sched_setscheduler, sched_setparam, sched_setattr with RT policies
    // For simplicity, block these syscalls entirely
    #[cfg(target_arch = "x86_64")]
    {
        let sched_setscheduler = 144i64;
        let sched_setparam = 142i64;
        let sched_setattr = 314i64;

        for syscall in [sched_setscheduler, sched_setparam, sched_setattr] {
            let rule = SeccompRule::new(vec![]).map_err(|e| e.to_string())?;
            rules.entry(syscall).or_default().push(rule);
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        let sched_setscheduler = 119i64;
        let sched_setparam = 118i64;
        let sched_setattr = 274i64;

        for syscall in [sched_setscheduler, sched_setparam, sched_setattr] {
            let rule = SeccompRule::new(vec![]).map_err(|e| e.to_string())?;
            rules.entry(syscall).or_default().push(rule);
        }
    }

    log::debug!("RestrictRealtime: blocking RT scheduling syscalls");
    Ok(())
}

/// Add seccomp rules for ProtectClock
fn add_protect_clock_rules(rules: &mut BTreeMap<i64, Vec<SeccompRule>>) -> Result<(), String> {
    // Block clock modification syscalls
    let clock_syscalls = ["clock_settime", "clock_adjtime", "settimeofday"];

    for name in clock_syscalls {
        if let Some(nr) = syscall_name_to_nr(name) {
            let rule = SeccompRule::new(vec![]).map_err(|e| e.to_string())?;
            rules.entry(nr).or_default().push(rule);
        }
    }

    log::debug!("ProtectClock: blocking clock modification syscalls");
    Ok(())
}

/// Add seccomp rules for ProtectHostname
fn add_protect_hostname_rules(rules: &mut BTreeMap<i64, Vec<SeccompRule>>) -> Result<(), String> {
    // Block hostname modification syscalls
    let hostname_syscalls = ["sethostname", "setdomainname"];

    for name in hostname_syscalls {
        if let Some(nr) = syscall_name_to_nr(name) {
            let rule = SeccompRule::new(vec![]).map_err(|e| e.to_string())?;
            rules.entry(nr).or_default().push(rule);
        }
    }

    log::debug!("ProtectHostname: blocking hostname modification syscalls");
    Ok(())
}

/// Add seccomp rules for LockPersonality
fn add_lock_personality_rules(rules: &mut BTreeMap<i64, Vec<SeccompRule>>) -> Result<(), String> {
    // Block personality() syscall
    #[cfg(target_arch = "x86_64")]
    let personality_nr = 135i64;
    #[cfg(target_arch = "aarch64")]
    let personality_nr = 92i64;

    let rule = SeccompRule::new(vec![]).map_err(|e| e.to_string())?;
    rules.entry(personality_nr).or_default().push(rule);

    log::debug!("LockPersonality: blocking personality() syscall");
    Ok(())
}

/// Add seccomp rules for RestrictSUIDSGID
fn add_restrict_suid_sgid_rules(rules: &mut BTreeMap<i64, Vec<SeccompRule>>) -> Result<(), String> {
    #[cfg(target_arch = "x86_64")]
    {
        add_suid_sgid_rules_x86_64(rules)?;
    }

    #[cfg(target_arch = "aarch64")]
    {
        add_suid_sgid_rules_aarch64(rules)?;
    }

    log::debug!("RestrictSUIDSGID: blocking SUID/SGID file creation");
    Ok(())
}

fn add_suid_sgid_rules_x86_64(rules: &mut BTreeMap<i64, Vec<SeccompRule>>) -> Result<(), String> {
    let suid = libc::S_ISUID as u64;
    let sgid = libc::S_ISGID as u64;
    add_mode_match_rule(rules, 90, 1, suid | sgid)?;
    add_mode_match_rule(rules, 91, 1, suid)?;
    add_mode_match_rule(rules, 91, 1, sgid)?;
    add_mode_match_rule(rules, 268, 2, suid)?;
    add_mode_match_rule(rules, 268, 2, sgid)?;
    Ok(())
}

#[cfg(target_arch = "aarch64")]
fn add_suid_sgid_rules_aarch64(rules: &mut BTreeMap<i64, Vec<SeccompRule>>) -> Result<(), String> {
    let suid = libc::S_ISUID as u64;
    let sgid = libc::S_ISGID as u64;
    add_mode_match_rule(rules, 52, 1, suid)?;
    add_mode_match_rule(rules, 52, 1, sgid)?;
    add_mode_match_rule(rules, 53, 2, suid)?;
    add_mode_match_rule(rules, 53, 2, sgid)?;
    Ok(())
}

fn add_mode_match_rule(
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
    syscall_nr: i64,
    arg_index: u8,
    bit_mask: u64,
) -> Result<(), String> {
    let condition = SeccompCondition::new(
        arg_index,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::MaskedEq(bit_mask),
        bit_mask,
    )
    .map_err(|e| e.to_string())?;
    let rule = SeccompRule::new(vec![condition]).map_err(|e| e.to_string())?;
    rules.entry(syscall_nr).or_default().push(rule);
    Ok(())
}

fn add_unconditional_rule(
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
    syscall_nr: i64,
) -> Result<(), String> {
    let rule = SeccompRule::new(vec![]).map_err(|e| e.to_string())?;
    rules.entry(syscall_nr).or_default().push(rule);
    Ok(())
}

/// Add seccomp rules for RestrictAddressFamilies
fn add_restrict_address_families_rules(
    rules: &mut BTreeMap<i64, Vec<SeccompRule>>,
    families: &[String],
) -> Result<(), String> {
    // Parse families and determine if it's an allow or deny list
    let is_deny = families.iter().any(|f| f.starts_with('~'));

    // Get socket syscall numbers
    #[cfg(target_arch = "x86_64")]
    let socket_nr = 41i64;
    #[cfg(target_arch = "aarch64")]
    let socket_nr = 198i64;

    // Map family names to constants
    let family_map: &[(&str, u64)] = &[
        ("AF_UNIX", libc::AF_UNIX as u64),
        ("AF_LOCAL", libc::AF_LOCAL as u64),
        ("AF_INET", libc::AF_INET as u64),
        ("AF_INET6", libc::AF_INET6 as u64),
        ("AF_NETLINK", libc::AF_NETLINK as u64),
        ("AF_PACKET", libc::AF_PACKET as u64),
    ];

    if is_deny {
        // Deny list - block specified families
        for family_str in families {
            let name = family_str.strip_prefix('~').unwrap_or(family_str);
            if let Some((_, af)) = family_map
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
            {
                // Block socket(af, ..., ...)
                if let Ok(cond) = SeccompCondition::new(
                    0, // arg0 = domain/family
                    SeccompCmpArgLen::Dword,
                    SeccompCmpOp::Eq,
                    *af,
                ) {
                    let rule = SeccompRule::new(vec![cond]).map_err(|e| e.to_string())?;
                    rules.entry(socket_nr).or_default().push(rule);
                }
            }
        }
    } else {
        // Allow list - block everything except specified families
        // This requires blocking all families and then allowing specific ones
        // seccompiler uses allow-by-default, so we'd need to invert the logic
        // For now, just log a warning
        log::warn!("RestrictAddressFamilies allow list not fully supported, use ~AF_XXX to deny");
    }

    log::debug!("RestrictAddressFamilies: filtering socket() calls");
    Ok(())
}

/// Convert syscall name to number
fn syscall_name_to_nr(name: &str) -> Option<i64> {
    #[cfg(target_arch = "x86_64")]
    {
        return lookup_syscall_nr(SYSCALL_NR_X86_64, name);
    }
    #[cfg(target_arch = "aarch64")]
    {
        return lookup_syscall_nr(SYSCALL_NR_AARCH64, name);
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = name;
        None
    }
}

fn lookup_syscall_nr(entries: &[(&str, i64)], name: &str) -> Option<i64> {
    entries
        .iter()
        .find_map(|(syscall_name, nr)| (*syscall_name == name).then_some(*nr))
}
