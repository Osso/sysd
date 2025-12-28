//! Unit file parsing and type definitions
//!
//! Parses systemd .service files into typed Rust structures.

mod parser;
mod service;

pub use parser::{parse_file, parse_unit_file, ParseError, ParsedFile};
pub use service::*;

use std::path::Path;

/// Convert parsed INI data into a typed Service
pub fn parse_service(name: &str, parsed: &ParsedFile) -> Result<Service, ParseError> {
    let mut svc = Service::new(name.to_string());

    // [Unit] section
    if let Some(unit) = parsed.get("[Unit]") {
        if let Some(vals) = unit.get("DESCRIPTION") {
            svc.unit.description = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = unit.get("AFTER") {
            svc.unit.after = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("BEFORE") {
            svc.unit.before = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("REQUIRES") {
            svc.unit.requires = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("WANTS") {
            svc.unit.wants = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONFLICTS") {
            svc.unit.conflicts = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = unit.get("CONDITIONPATHEXISTS") {
            svc.unit.condition_path_exists = vals.iter().map(|(_, v)| v.clone()).collect();
        }
    }

    // [Service] section
    if let Some(service) = parsed.get("[Service]") {
        // Type
        if let Some(vals) = service.get("TYPE") {
            if let Some((_, t)) = vals.first() {
                svc.service.service_type =
                    ServiceType::parse(t).unwrap_or_default();
            }
        }

        // Exec commands
        if let Some(vals) = service.get("EXECSTART") {
            svc.service.exec_start = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = service.get("EXECSTARTPRE") {
            svc.service.exec_start_pre = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = service.get("EXECSTARTPOST") {
            svc.service.exec_start_post = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = service.get("EXECSTOP") {
            svc.service.exec_stop = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = service.get("EXECRELOAD") {
            svc.service.exec_reload = vals.iter().map(|(_, v)| v.clone()).collect();
        }

        // Restart
        if let Some(vals) = service.get("RESTART") {
            if let Some((_, r)) = vals.first() {
                svc.service.restart = RestartPolicy::parse(r).unwrap_or_default();
            }
        }
        if let Some(vals) = service.get("RESTARTSEC") {
            if let Some((_, s)) = vals.first() {
                svc.service.restart_sec =
                    parse_duration(s).unwrap_or(std::time::Duration::from_millis(100));
            }
        }
        if let Some(vals) = service.get("TIMEOUTSTARTSEC") {
            if let Some((_, s)) = vals.first() {
                svc.service.timeout_start_sec = parse_duration(s);
            }
        }
        if let Some(vals) = service.get("TIMEOUTSTOPSEC") {
            if let Some((_, s)) = vals.first() {
                svc.service.timeout_stop_sec = parse_duration(s);
            }
        }

        // Credentials
        if let Some(vals) = service.get("USER") {
            svc.service.user = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = service.get("GROUP") {
            svc.service.group = vals.first().map(|(_, v)| v.clone());
        }
        if let Some(vals) = service.get("WORKINGDIRECTORY") {
            svc.service.working_directory = vals.first().map(|(_, v)| v.into());
        }

        // Environment
        if let Some(vals) = service.get("ENVIRONMENT") {
            for (_, v) in vals {
                if let Ok(pairs) = parser::parse_environment(v) {
                    svc.service.environment.extend(pairs);
                }
            }
        }
        if let Some(vals) = service.get("ENVIRONMENTFILE") {
            svc.service.environment_file = vals.iter().map(|(_, v)| v.into()).collect();
        }

        // I/O
        if let Some(vals) = service.get("STANDARDOUTPUT") {
            if let Some((_, s)) = vals.first() {
                svc.service.standard_output = StdOutput::parse(s).unwrap_or_default();
            }
        }
        if let Some(vals) = service.get("STANDARDERROR") {
            if let Some((_, s)) = vals.first() {
                svc.service.standard_error = StdOutput::parse(s).unwrap_or_default();
            }
        }

        // Resource limits
        if let Some(vals) = service.get("MEMORYMAX") {
            if let Some((_, s)) = vals.first() {
                svc.service.memory_max = parse_memory(s);
            }
        }
        if let Some(vals) = service.get("CPUQUOTA") {
            if let Some((_, s)) = vals.first() {
                svc.service.cpu_quota = parse_cpu_quota(s);
            }
        }
        if let Some(vals) = service.get("TASKSMAX") {
            if let Some((_, s)) = vals.first() {
                svc.service.tasks_max = s.parse().ok();
            }
        }
    }

    // [Install] section
    if let Some(install) = parsed.get("[Install]") {
        if let Some(vals) = install.get("WANTEDBY") {
            svc.install.wanted_by = vals.iter().map(|(_, v)| v.clone()).collect();
        }
        if let Some(vals) = install.get("REQUIREDBY") {
            svc.install.required_by = vals.iter().map(|(_, v)| v.clone()).collect();
        }
    }

    Ok(svc)
}

/// Parse a service file from disk
pub async fn load_service(path: &Path) -> Result<Service, ParseError> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let parsed = parse_unit_file(path).await?;
    parse_service(name, &parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_docker_service() {
        let content = r#"
[Unit]
Description=Docker Application Container Engine
After=network-online.target docker.socket firewalld.service
Wants=network-online.target
Requires=docker.socket

[Service]
Type=notify
ExecStart=/usr/bin/dockerd -H fd://
ExecReload=/bin/kill -s HUP $MAINPID
TimeoutStartSec=0
RestartSec=2
Restart=always
MemoryMax=2G

[Install]
WantedBy=multi-user.target
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("docker.service", &parsed).unwrap();

        assert_eq!(svc.unit.description, Some("Docker Application Container Engine".into()));
        assert!(svc.unit.after.contains(&"network-online.target".into()));
        assert!(svc.unit.wants.contains(&"network-online.target".into()));
        assert!(svc.unit.requires.contains(&"docker.socket".into()));

        assert_eq!(svc.service.service_type, ServiceType::Notify);
        assert_eq!(svc.service.restart, RestartPolicy::Always);
        assert_eq!(svc.service.restart_sec, std::time::Duration::from_secs(2));
        assert_eq!(svc.service.memory_max, Some(2 * 1024 * 1024 * 1024));

        assert!(svc.install.wanted_by.contains(&"multi-user.target".into()));
    }

    #[test]
    fn test_parse_simple_service() {
        let content = r#"
[Unit]
Description=My App

[Service]
Type=simple
ExecStart=/usr/bin/myapp --flag
User=nobody
WorkingDirectory=/var/lib/myapp
Environment=FOO=bar BAZ=qux

[Install]
WantedBy=multi-user.target
"#;
        let parsed = parse_file(content).unwrap();
        let svc = parse_service("myapp.service", &parsed).unwrap();

        assert_eq!(svc.service.service_type, ServiceType::Simple);
        assert_eq!(svc.service.user, Some("nobody".into()));
        assert_eq!(
            svc.service.working_directory,
            Some("/var/lib/myapp".into())
        );
        assert!(svc.service.environment.contains(&("FOO".into(), "bar".into())));
        assert!(svc.service.environment.contains(&("BAZ".into(), "qux".into())));
    }
}
