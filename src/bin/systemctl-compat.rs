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

struct ParsedArgs {
    user_mode: bool,
    quiet: bool,
    wait: bool,
    job_mode: Option<String>,
    command: String,
    positional: Vec<String>,
}

fn main() {
    let parsed = parse_args(env::args().skip(1).collect());
    let sysdctl_args = build_sysdctl_args(parsed);

    // Execute sysdctl
    let err = Command::new("sysdctl").args(&sysdctl_args).exec();

    // exec() only returns on error
    eprintln!("systemctl-compat: failed to exec sysdctl: {}", err);
    exit(1);
}

fn parse_args(args: Vec<String>) -> ParsedArgs {
    if args.is_empty() {
        eprintln!("systemctl-compat: no command specified");
        exit(1);
    }

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
            s if s.starts_with('-') => {}
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

    let command = command.unwrap_or_else(|| {
        eprintln!("systemctl-compat: no command specified");
        exit(1);
    });

    ParsedArgs {
        user_mode,
        quiet,
        wait,
        job_mode,
        command,
        positional,
    }
}

fn build_sysdctl_args(parsed: ParsedArgs) -> Vec<String> {
    let mut sysdctl_args: Vec<String> = Vec::new();
    if parsed.user_mode {
        sysdctl_args.push("--user".to_string());
    }

    match parsed.command.as_str() {
        "is-active" => {
            sysdctl_args.push("is-active".to_string());
            if parsed.quiet {
                sysdctl_args.push("--quiet".to_string());
            }
            push_required_unit(&mut sysdctl_args, &parsed.positional, "is-active");
        }
        "reset-failed" => {
            sysdctl_args.push("reset-failed".to_string());
        }
        "import-environment" => {
            sysdctl_args.push("import-environment".to_string());
        }
        "start" => {
            sysdctl_args.push("start".to_string());
            if parsed.wait {
                sysdctl_args.push("--wait".to_string());
            }
            if let Some(mode) = parsed.job_mode {
                sysdctl_args.push(format!("--job-mode={}", mode));
            }
            push_required_unit(&mut sysdctl_args, &parsed.positional, "start");
        }
        "stop" => {
            sysdctl_args.push("stop".to_string());
            push_required_unit(&mut sysdctl_args, &parsed.positional, "stop");
        }
        "restart" => {
            sysdctl_args.push("restart".to_string());
            push_required_unit(&mut sysdctl_args, &parsed.positional, "restart");
        }
        "status" => {
            sysdctl_args.push("status".to_string());
            push_required_unit(&mut sysdctl_args, &parsed.positional, "status");
        }
        "unset-environment" => {
            sysdctl_args.push("unset-environment".to_string());
            sysdctl_args.extend(parsed.positional);
        }
        "daemon-reload" => {
            sysdctl_args.push("reload".to_string());
        }
        "enable" => {
            sysdctl_args.push("enable".to_string());
            push_optional_unit(&mut sysdctl_args, &parsed.positional);
        }
        "disable" => {
            sysdctl_args.push("disable".to_string());
            push_optional_unit(&mut sysdctl_args, &parsed.positional);
        }
        "is-enabled" => {
            sysdctl_args.push("is-enabled".to_string());
            push_optional_unit(&mut sysdctl_args, &parsed.positional);
        }
        _ => unsupported_command(&parsed.command),
    }

    sysdctl_args
}

fn push_required_unit(args: &mut Vec<String>, positional: &[String], command: &str) {
    if let Some(unit) = positional.first() {
        args.push(unit.clone());
        return;
    }
    eprintln!("systemctl-compat: {} requires unit name", command);
    exit(1);
}

fn push_optional_unit(args: &mut Vec<String>, positional: &[String]) {
    if let Some(unit) = positional.first() {
        args.push(unit.clone());
    }
}

fn unsupported_command(command: &str) -> ! {
    eprintln!("systemctl-compat: unsupported command '{}'", command);
    eprintln!(
        "Supported: is-active, reset-failed, import-environment, start, stop, restart, status, unset-environment, daemon-reload, enable, disable, is-enabled"
    );
    exit(1);
}
