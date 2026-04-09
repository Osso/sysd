//! systemctl compatibility wrapper for sysd
//!
//! Translates systemctl commands to sysdctl equivalents so that scripts
//! expecting systemctl (like niri-session) work with sysd.
//!
//! Supported commands (subset used by niri-session, etc.):
//! - systemctl --user is-active [-q] <unit>
//! - systemctl --user reset-failed
//! - systemctl --user import-environment
//! - systemctl --user start [--wait] [--job-mode=...] <unit>
//! - systemctl --user unset-environment <vars...>
//! - systemctl --user stop <unit>
//! - systemctl --user restart <unit>
//! - systemctl --user status <unit>

use std::env;
use std::os::unix::process::CommandExt;
use std::process::{exit, Command};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    if args.is_empty() {
        eprintln!("systemctl-compat: no command specified");
        exit(1);
    }

    // Parse arguments
    let mut user_mode = false;
    let mut quiet = false;
    let mut wait = false;
    let mut job_mode: Option<String> = None;
    let mut command: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--user" => user_mode = true,
            "-q" | "--quiet" => quiet = true,
            "--wait" => wait = true,
            s if s.starts_with("--job-mode=") => {
                job_mode = Some(s.strip_prefix("--job-mode=").unwrap().to_string());
            }
            "--job-mode" => {
                i += 1;
                if i < args.len() {
                    job_mode = Some(args[i].clone());
                }
            }
            s if s.starts_with("-") => {
                // Ignore other flags we don't handle
            }
            _ => {
                if command.is_none() {
                    command = Some(arg.clone());
                } else {
                    positional.push(arg.clone());
                }
            }
        }
        i += 1;
    }

    let command = match command {
        Some(c) => c,
        None => {
            eprintln!("systemctl-compat: no command specified");
            exit(1);
        }
    };

    // Build sysdctl command
    let mut sysdctl_args: Vec<String> = Vec::new();

    if user_mode {
        sysdctl_args.push("--user".to_string());
    }

    match command.as_str() {
        "is-active" => {
            sysdctl_args.push("is-active".to_string());
            if quiet {
                sysdctl_args.push("--quiet".to_string());
            }
            if let Some(unit) = positional.first() {
                sysdctl_args.push(unit.clone());
            } else {
                eprintln!("systemctl-compat: is-active requires unit name");
                exit(1);
            }
        }
        "reset-failed" => {
            sysdctl_args.push("reset-failed".to_string());
        }
        "import-environment" => {
            sysdctl_args.push("import-environment".to_string());
        }
        "start" => {
            sysdctl_args.push("start".to_string());
            if wait {
                sysdctl_args.push("--wait".to_string());
            }
            if let Some(mode) = job_mode {
                sysdctl_args.push(format!("--job-mode={}", mode));
            }
            if let Some(unit) = positional.first() {
                sysdctl_args.push(unit.clone());
            } else {
                eprintln!("systemctl-compat: start requires unit name");
                exit(1);
            }
        }
        "stop" => {
            sysdctl_args.push("stop".to_string());
            if let Some(unit) = positional.first() {
                sysdctl_args.push(unit.clone());
            } else {
                eprintln!("systemctl-compat: stop requires unit name");
                exit(1);
            }
        }
        "restart" => {
            sysdctl_args.push("restart".to_string());
            if let Some(unit) = positional.first() {
                sysdctl_args.push(unit.clone());
            } else {
                eprintln!("systemctl-compat: restart requires unit name");
                exit(1);
            }
        }
        "status" => {
            sysdctl_args.push("status".to_string());
            if let Some(unit) = positional.first() {
                sysdctl_args.push(unit.clone());
            } else {
                eprintln!("systemctl-compat: status requires unit name");
                exit(1);
            }
        }
        "unset-environment" => {
            sysdctl_args.push("unset-environment".to_string());
            sysdctl_args.extend(positional);
        }
        "daemon-reload" => {
            // Map to reload
            sysdctl_args.push("reload".to_string());
        }
        "enable" => {
            sysdctl_args.push("enable".to_string());
            if let Some(unit) = positional.first() {
                sysdctl_args.push(unit.clone());
            }
        }
        "disable" => {
            sysdctl_args.push("disable".to_string());
            if let Some(unit) = positional.first() {
                sysdctl_args.push(unit.clone());
            }
        }
        "is-enabled" => {
            sysdctl_args.push("is-enabled".to_string());
            if let Some(unit) = positional.first() {
                sysdctl_args.push(unit.clone());
            }
        }
        _ => {
            eprintln!("systemctl-compat: unsupported command '{}'", command);
            eprintln!("Supported: is-active, reset-failed, import-environment, start, stop, restart, status, unset-environment, daemon-reload, enable, disable, is-enabled");
            exit(1);
        }
    }

    // Execute sysdctl
    let err = Command::new("sysdctl").args(&sysdctl_args).exec();

    // exec() only returns on error
    eprintln!("systemctl-compat: failed to exec sysdctl: {}", err);
    exit(1);
}
