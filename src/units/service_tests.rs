use super::*;

// ServiceType tests
#[test]
fn test_service_type_parse() {
    assert_eq!(ServiceType::parse("simple"), Some(ServiceType::Simple));
    assert_eq!(ServiceType::parse("SIMPLE"), Some(ServiceType::Simple));
    assert_eq!(ServiceType::parse("Simple"), Some(ServiceType::Simple));
    assert_eq!(ServiceType::parse("forking"), Some(ServiceType::Forking));
    assert_eq!(ServiceType::parse("notify"), Some(ServiceType::Notify));
    // notify-reload is used by dbus-broker - treat as Notify
    assert_eq!(
        ServiceType::parse("notify-reload"),
        Some(ServiceType::Notify)
    );
    assert_eq!(ServiceType::parse("dbus"), Some(ServiceType::Dbus));
    assert_eq!(ServiceType::parse("oneshot"), Some(ServiceType::Oneshot));
    assert_eq!(ServiceType::parse("invalid"), None);
    assert_eq!(ServiceType::parse(""), None);
}

#[test]
fn test_service_type_default() {
    assert_eq!(ServiceType::default(), ServiceType::Simple);
}

// RestartPolicy tests
#[test]
fn test_restart_policy_parse() {
    assert_eq!(RestartPolicy::parse("no"), Some(RestartPolicy::No));
    assert_eq!(RestartPolicy::parse("NO"), Some(RestartPolicy::No));
    assert_eq!(
        RestartPolicy::parse("on-failure"),
        Some(RestartPolicy::OnFailure)
    );
    assert_eq!(
        RestartPolicy::parse("ON-FAILURE"),
        Some(RestartPolicy::OnFailure)
    );
    assert_eq!(RestartPolicy::parse("always"), Some(RestartPolicy::Always));
    assert_eq!(RestartPolicy::parse("ALWAYS"), Some(RestartPolicy::Always));
    assert_eq!(RestartPolicy::parse("invalid"), None);
    assert_eq!(RestartPolicy::parse(""), None);
}

#[test]
fn test_restart_policy_default() {
    assert_eq!(RestartPolicy::default(), RestartPolicy::No);
}

// StdOutput tests
#[test]
fn test_std_output_parse() {
    assert_eq!(StdOutput::parse("journal"), Some(StdOutput::Journal));
    assert_eq!(StdOutput::parse("JOURNAL"), Some(StdOutput::Journal));
    assert_eq!(StdOutput::parse("inherit"), Some(StdOutput::Inherit));
    assert_eq!(StdOutput::parse("null"), Some(StdOutput::Null));
    assert_eq!(StdOutput::parse("/dev/null"), Some(StdOutput::Null));
    assert_eq!(StdOutput::parse("invalid"), None);
}

#[test]
fn kill_input_device_and_proc_modes_parse_all_values() {
    assert_eq!(
        KillMode::parse("control-group"),
        Some(KillMode::ControlGroup)
    );
    assert_eq!(KillMode::parse("process"), Some(KillMode::Process));
    assert_eq!(KillMode::parse("mixed"), Some(KillMode::Mixed));
    assert_eq!(KillMode::parse("none"), Some(KillMode::None));
    assert_eq!(KillMode::parse("bad"), None);

    assert_eq!(StdInput::parse("null"), Some(StdInput::Null));
    assert_eq!(StdInput::parse("/dev/null"), Some(StdInput::Null));
    assert_eq!(StdInput::parse("tty"), Some(StdInput::Tty));
    assert_eq!(StdInput::parse("tty-force"), Some(StdInput::TtyForce));
    assert_eq!(StdInput::parse("tty-fail"), Some(StdInput::TtyFail));
    assert_eq!(StdInput::parse("pipe"), None);

    assert_eq!(DevicePolicy::parse("auto"), Some(DevicePolicy::Auto));
    assert_eq!(DevicePolicy::parse("closed"), Some(DevicePolicy::Closed));
    assert_eq!(DevicePolicy::parse("strict"), Some(DevicePolicy::Strict));
    assert_eq!(DevicePolicy::parse("unknown"), None);

    assert_eq!(ProtectHome::parse("tmpfs"), Some(ProtectHome::Tmpfs));
    assert_eq!(ProtectHome::parse("unknown"), None);

    assert_eq!(
        ProtectProc::parse("ptraceable"),
        Some(ProtectProc::Ptraceable)
    );
    assert_eq!(ProtectProc::parse("noaccess"), Some(ProtectProc::NoAccess));
    assert_eq!(ProtectProc::parse("unknown"), None);
}

#[test]
fn test_std_output_default() {
    assert_eq!(StdOutput::default(), StdOutput::Journal);
}

// Duration parsing tests
#[test]
fn test_parse_duration() {
    assert_eq!(parse_duration("5s"), Some(Duration::from_secs(5)));
    assert_eq!(parse_duration("100ms"), Some(Duration::from_millis(100)));
    assert_eq!(parse_duration("2min"), Some(Duration::from_secs(120)));
    assert_eq!(parse_duration("3sec"), Some(Duration::from_secs(3)));
    assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
    assert_eq!(parse_duration("2d"), Some(Duration::from_secs(2 * 86400)));
    assert_eq!(
        parse_duration("2w"),
        Some(Duration::from_secs(2 * 7 * 86400))
    );
    assert_eq!(
        parse_duration("2week"),
        Some(Duration::from_secs(2 * 7 * 86400))
    );
    assert_eq!(parse_duration("30"), Some(Duration::from_secs(30)));
}

#[test]
fn test_parse_duration_edge_cases() {
    assert_eq!(parse_duration("0"), Some(Duration::from_secs(0)));
    assert_eq!(parse_duration("0s"), Some(Duration::from_secs(0)));
    assert_eq!(parse_duration("0ms"), Some(Duration::from_millis(0)));
    assert_eq!(parse_duration("  5s  "), Some(Duration::from_secs(5)));
    assert_eq!(parse_duration("invalid"), None);
    assert_eq!(parse_duration(""), None);
    assert_eq!(parse_duration("5x"), None);
}

// Memory parsing tests
#[test]
fn test_parse_memory() {
    assert_eq!(parse_memory("1G"), Some(1024 * 1024 * 1024));
    assert_eq!(parse_memory("512M"), Some(512 * 1024 * 1024));
    assert_eq!(parse_memory("1024K"), Some(1024 * 1024));
    assert_eq!(parse_memory("1048576"), Some(1048576));
}

#[test]
fn test_parse_memory_edge_cases() {
    assert_eq!(parse_memory("0"), Some(0));
    assert_eq!(parse_memory("  1G  "), Some(1024 * 1024 * 1024));
    assert_eq!(parse_memory("invalid"), None);
    assert_eq!(parse_memory(""), None);
    assert_eq!(parse_memory("1T"), None); // T not supported
}

// CPU quota tests
#[test]
fn test_parse_cpu_quota() {
    assert_eq!(parse_cpu_quota("50%"), Some(50));
    assert_eq!(parse_cpu_quota("200%"), Some(200));
    assert_eq!(parse_cpu_quota("0%"), Some(0));
    assert_eq!(parse_cpu_quota("100"), None);
    assert_eq!(parse_cpu_quota(""), None);
    assert_eq!(parse_cpu_quota("invalid%"), None);
}

// ServiceSection default tests
#[test]
fn test_service_section_default() {
    let section = ServiceSection::default();
    assert_eq!(section.service_type, ServiceType::Simple);
    assert_eq!(section.restart, RestartPolicy::No);
    assert_eq!(section.restart_sec, Duration::from_millis(100));
    assert!(section.exec_start.is_empty());
    assert!(section.user.is_none());
}

// Service tests
#[test]
fn test_service_new() {
    let svc = Service::new("test.service".to_string());
    assert_eq!(svc.name, "test.service");
    assert!(svc.unit.description.is_none());
    assert!(svc.unit.after.is_empty());
    assert!(svc.install.wanted_by.is_empty());
}

// Template unit tests
#[test]
fn test_extract_instance() {
    assert_eq!(extract_instance("foo@bar.service"), Some("bar".to_string()));
    assert_eq!(
        extract_instance("getty@tty1.service"),
        Some("tty1".to_string())
    );
    assert_eq!(extract_instance("foo@.service"), None); // Template file
    assert_eq!(extract_instance("foo.service"), None); // Not a template
    assert_eq!(extract_instance("foo@bar"), Some("bar".to_string()));
}

#[test]
fn test_get_template_name() {
    assert_eq!(
        get_template_name("foo@bar.service"),
        Some("foo@.service".to_string())
    );
    assert_eq!(
        get_template_name("getty@tty1.service"),
        Some("getty@.service".to_string())
    );
    assert_eq!(
        get_template_name("foo@.service"),
        Some("foo@.service".to_string())
    );
    assert_eq!(get_template_name("foo.service"), None); // Not a template
}

#[test]
fn test_service_new_with_instance() {
    let svc = Service::new("getty@tty1.service".to_string());
    assert_eq!(svc.name, "getty@tty1.service");
    assert_eq!(svc.instance, Some("tty1".to_string()));

    let svc2 = Service::new("foo.service".to_string());
    assert_eq!(svc2.instance, None);
}

#[test]
fn test_is_bare_template() {
    assert!(is_bare_template("foo@.service"));
    assert!(is_bare_template("getty@.service"));
    assert!(!is_bare_template("foo@bar.service"));
    assert!(!is_bare_template("foo.service"));
}

#[test]
fn test_instantiate_template() {
    assert_eq!(
        instantiate_template("foo@.service", "bar"),
        Some("foo@bar.service".to_string())
    );
    assert_eq!(
        instantiate_template("getty@.service", "tty1"),
        Some("getty@tty1.service".to_string())
    );
    // Not a bare template - returns None
    assert_eq!(instantiate_template("foo@bar.service", "baz"), None);
    assert_eq!(instantiate_template("foo.service", "bar"), None);
}
