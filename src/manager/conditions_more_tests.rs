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
