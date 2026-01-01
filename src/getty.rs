//! Getty generator - creates getty units from kernel console= parameters
//!
//! Replaces systemd-getty-generator with built-in parsing.
//!
//! Parses /proc/cmdline for console= parameters and creates:
//! - serial-getty@ttyS0.service for serial consoles
//! - getty@tty1.service for virtual consoles

use std::path::{Path, PathBuf};

use crate::units::{InstallSection, Service, ServiceType, StdInput, StdOutput};

/// Parsed console parameter
#[derive(Debug, Clone)]
pub struct ConsoleParam {
    /// TTY device name (e.g., "ttyS0", "tty1")
    pub tty: String,
    /// Baud rate for serial consoles (e.g., 115200)
    pub baud: Option<u32>,
    /// Additional options
    pub options: Option<String>,
}

impl ConsoleParam {
    /// Check if this is a serial console
    pub fn is_serial(&self) -> bool {
        self.tty.starts_with("ttyS")
            || self.tty.starts_with("ttyUSB")
            || self.tty.starts_with("ttyAMA") // Raspberry Pi
            || self.tty.starts_with("ttyO") // OMAP
            || self.tty.starts_with("ttymxc") // i.MX
            || self.tty.starts_with("ttyPS") // Xilinx
    }

    /// Check if this is a virtual console
    pub fn is_virtual(&self) -> bool {
        // tty0 is "current console", tty1-63 are virtual consoles
        if let Some(rest) = self.tty.strip_prefix("tty") {
            rest.parse::<u32>().is_ok()
        } else {
            false
        }
    }

    /// Get the service unit name for this console
    pub fn service_name(&self) -> String {
        if self.is_serial() {
            format!("serial-getty@{}.service", self.tty)
        } else {
            format!("getty@{}.service", self.tty)
        }
    }

    /// Create a getty service unit for this console
    pub fn to_service(&self) -> Service {
        let name = self.service_name();
        let mut svc = Service::new(name);

        // Unit section
        svc.unit.description = Some(format!("Getty on {}", self.tty));
        svc.unit.after.push("systemd-user-sessions.service".to_string());
        svc.unit.after.push("plymouth-quit-wait.service".to_string());

        // For serial consoles, also wait for dev node
        if self.is_serial() {
            svc.unit.after.push(format!("dev-{}.device", self.tty));
            // Use requires instead of binds_to (which we don't have)
            svc.unit.requires.push(format!("dev-{}.device", self.tty));
        }

        // Service section
        svc.service.service_type = ServiceType::Idle;
        svc.service.restart = crate::units::RestartPolicy::Always;
        svc.service.restart_sec = std::time::Duration::from_secs(0);

        // Build agetty command
        let exec_start = if self.is_serial() {
            if let Some(baud) = self.baud {
                format!("/sbin/agetty -o '-p -- \\\\u' --keep-baud {} {} $TERM", baud, self.tty)
            } else {
                format!("/sbin/agetty -o '-p -- \\\\u' --keep-baud 115200,57600,38400,9600 {} $TERM", self.tty)
            }
        } else {
            format!("/sbin/agetty -o '-p -- \\\\u' --noclear {} $TERM", self.tty)
        };
        svc.service.exec_start = vec![exec_start];

        // TTY settings
        svc.service.tty_path = Some(PathBuf::from(format!("/dev/{}", self.tty)));
        svc.service.tty_reset = true;

        // Standard streams - use TTY
        svc.service.standard_input = StdInput::Tty;
        svc.service.standard_output = StdOutput::Inherit;

        // Install section
        svc.install = InstallSection {
            wanted_by: vec!["getty.target".to_string()],
            ..Default::default()
        };

        svc
    }
}

/// Parse kernel command line for console= parameters
pub fn parse_cmdline(cmdline: &str) -> Vec<ConsoleParam> {
    cmdline
        .split_whitespace()
        .filter_map(|param| {
            let value = param.strip_prefix("console=")?;
            parse_console_param(value)
        })
        .collect()
}

/// Parse a single console= value (e.g., "ttyS0,115200n8")
fn parse_console_param(value: &str) -> Option<ConsoleParam> {
    // Format: tty[,baudrate[options]]
    // Examples: ttyS0, ttyS0,115200, ttyS0,115200n8, tty0

    let parts: Vec<&str> = value.split(',').collect();
    let tty = parts.first()?.to_string();

    if tty.is_empty() {
        return None;
    }

    let (baud, options) = if let Some(baud_str) = parts.get(1) {
        // Parse baud rate (may have trailing options like 'n8')
        let baud_part: String = baud_str.chars().take_while(|c| c.is_ascii_digit()).collect();
        let baud = baud_part.parse().ok();
        let opts = if baud_part.len() < baud_str.len() {
            Some(baud_str[baud_part.len()..].to_string())
        } else {
            None
        };
        (baud, opts)
    } else {
        (None, None)
    };

    Some(ConsoleParam { tty, baud, options })
}

/// Read and parse /proc/cmdline
pub fn parse_kernel_cmdline() -> std::io::Result<Vec<ConsoleParam>> {
    let cmdline = std::fs::read_to_string("/proc/cmdline")?;
    Ok(parse_cmdline(&cmdline))
}

/// Generate getty services from kernel command line
pub fn generate_getty_services(cmdline_path: &Path) -> std::io::Result<Vec<Service>> {
    let cmdline = std::fs::read_to_string(cmdline_path)?;
    let consoles = parse_cmdline(&cmdline);

    let services: Vec<Service> = consoles
        .into_iter()
        .map(|c| c.to_service())
        .collect();

    Ok(services)
}

/// Generate default virtual console gettys (tty1-tty6)
pub fn generate_default_gettys() -> Vec<Service> {
    (1..=6)
        .map(|n| {
            let param = ConsoleParam {
                tty: format!("tty{}", n),
                baud: None,
                options: None,
            };
            param.to_service()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_console_serial() {
        let params = parse_cmdline("console=ttyS0,115200");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].tty, "ttyS0");
        assert_eq!(params[0].baud, Some(115200));
        assert!(params[0].is_serial());
        assert!(!params[0].is_virtual());
    }

    #[test]
    fn test_parse_console_serial_with_options() {
        let params = parse_cmdline("console=ttyS0,115200n8");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].tty, "ttyS0");
        assert_eq!(params[0].baud, Some(115200));
        assert_eq!(params[0].options, Some("n8".to_string()));
    }

    #[test]
    fn test_parse_console_virtual() {
        let params = parse_cmdline("console=tty0");
        assert_eq!(params.len(), 1);
        assert_eq!(params[0].tty, "tty0");
        assert!(params[0].is_virtual());
        assert!(!params[0].is_serial());
    }

    #[test]
    fn test_parse_multiple_consoles() {
        let params = parse_cmdline("root=/dev/sda1 console=tty0 console=ttyS0,115200 quiet");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].tty, "tty0");
        assert_eq!(params[1].tty, "ttyS0");
    }

    #[test]
    fn test_parse_no_console() {
        let params = parse_cmdline("root=/dev/sda1 quiet");
        assert_eq!(params.len(), 0);
    }

    #[test]
    fn test_service_name_serial() {
        let param = ConsoleParam {
            tty: "ttyS0".to_string(),
            baud: Some(115200),
            options: None,
        };
        assert_eq!(param.service_name(), "serial-getty@ttyS0.service");
    }

    #[test]
    fn test_service_name_virtual() {
        let param = ConsoleParam {
            tty: "tty1".to_string(),
            baud: None,
            options: None,
        };
        assert_eq!(param.service_name(), "getty@tty1.service");
    }

    #[test]
    fn test_to_service_serial() {
        let param = ConsoleParam {
            tty: "ttyS0".to_string(),
            baud: Some(115200),
            options: None,
        };
        let svc = param.to_service();

        assert_eq!(svc.name, "serial-getty@ttyS0.service");
        assert!(svc.service.exec_start[0].contains("115200"));
        assert!(svc.service.exec_start[0].contains("ttyS0"));
        assert_eq!(svc.service.tty_path, Some(PathBuf::from("/dev/ttyS0")));
    }

    #[test]
    fn test_to_service_virtual() {
        let param = ConsoleParam {
            tty: "tty1".to_string(),
            baud: None,
            options: None,
        };
        let svc = param.to_service();

        assert_eq!(svc.name, "getty@tty1.service");
        assert!(svc.service.exec_start[0].contains("--noclear"));
        assert!(svc.service.exec_start[0].contains("tty1"));
    }

    #[test]
    fn test_generate_default_gettys() {
        let gettys = generate_default_gettys();
        assert_eq!(gettys.len(), 6);
        assert_eq!(gettys[0].name, "getty@tty1.service");
        assert_eq!(gettys[5].name, "getty@tty6.service");
    }

    #[test]
    fn test_is_serial_variants() {
        let cases = vec![
            ("ttyS0", true),
            ("ttyS1", true),
            ("ttyUSB0", true),
            ("ttyAMA0", true),  // Raspberry Pi
            ("ttyO0", true),    // OMAP
            ("ttymxc0", true),  // i.MX
            ("tty0", false),
            ("tty1", false),
            ("pts/0", false),
        ];

        for (tty, expected) in cases {
            let param = ConsoleParam {
                tty: tty.to_string(),
                baud: None,
                options: None,
            };
            assert_eq!(param.is_serial(), expected, "tty={}", tty);
        }
    }
}
