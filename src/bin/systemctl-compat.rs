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

    let mut state = ParseState::default();
    parse_argument_list(&args, &mut state);
    let command = state.command.unwrap_or_else(|| {
        eprintln!("systemctl-compat: no command specified");
        exit(1);
    });

    ParsedArgs {
        user_mode: state.user_mode,
        quiet: state.quiet,
        wait: state.wait,
        job_mode: state.job_mode,
        command,
        positional: state.positional,
    }
}

#[derive(Default)]
struct ParseState {
    user_mode: bool,
    quiet: bool,
    wait: bool,
    job_mode: Option<String>,
    command: Option<String>,
    positional: Vec<String>,
}

fn parse_argument_list(args: &[String], state: &mut ParseState) {
    let mut i = 0;
    while i < args.len() {
        i = parse_argument(args, i, state);
    }
}

fn parse_argument(args: &[String], i: usize, state: &mut ParseState) -> usize {
    let arg = &args[i];
    match arg.as_str() {
        "--user" => state.user_mode = true,
        "-q" | "--quiet" => state.quiet = true,
        "--wait" => state.wait = true,
        s if s.starts_with("--job-mode=") => {
            state.job_mode = Some(s.trim_start_matches("--job-mode=").to_string());
        }
        "--job-mode" => return parse_job_mode_value(args, i, state),
        s if s.starts_with('-') => {}
        _ => push_command_or_positional(arg, state),
    }
    i + 1
}

fn parse_job_mode_value(args: &[String], i: usize, state: &mut ParseState) -> usize {
    if i + 1 < args.len() {
        state.job_mode = Some(args[i + 1].clone());
        return i + 2;
    }
    i + 1
}

fn push_command_or_positional(arg: &str, state: &mut ParseState) {
    if state.command.is_none() {
        state.command = Some(arg.to_string());
        return;
    }
    state.positional.push(arg.to_string());
}

fn build_sysdctl_args(parsed: ParsedArgs) -> Vec<String> {
    let mut sysdctl_args = build_base_args(parsed.user_mode);
    append_command_args(&mut sysdctl_args, parsed);
    sysdctl_args
}

fn build_base_args(user_mode: bool) -> Vec<String> {
    let mut args = Vec::new();
    if user_mode {
        args.push("--user".to_string());
    }
    args
}

fn append_command_args(sysdctl_args: &mut Vec<String>, parsed: ParsedArgs) {
    match parsed.command.as_str() {
        "is-active" => append_is_active_args(sysdctl_args, &parsed),
        "reset-failed" => sysdctl_args.push("reset-failed".to_string()),
        "import-environment" => sysdctl_args.push("import-environment".to_string()),
        "start" => append_start_args(sysdctl_args, parsed),
        "stop" | "restart" | "status" => append_single_unit_action(sysdctl_args, &parsed),
        "unset-environment" => append_unset_environment_args(sysdctl_args, parsed),
        "daemon-reload" => sysdctl_args.push("reload".to_string()),
        "enable" | "disable" | "is-enabled" => append_optional_unit_action(sysdctl_args, parsed),
        _ => unsupported_command(&parsed.command),
    }
}

fn append_is_active_args(sysdctl_args: &mut Vec<String>, parsed: &ParsedArgs) {
    sysdctl_args.push("is-active".to_string());
    if parsed.quiet {
        sysdctl_args.push("--quiet".to_string());
    }
    push_required_unit(sysdctl_args, &parsed.positional, "is-active");
}

fn append_start_args(sysdctl_args: &mut Vec<String>, parsed: ParsedArgs) {
    sysdctl_args.push("start".to_string());
    if parsed.wait {
        sysdctl_args.push("--wait".to_string());
    }
    if let Some(mode) = parsed.job_mode {
        sysdctl_args.push(format!("--job-mode={}", mode));
    }
    push_required_unit(sysdctl_args, &parsed.positional, "start");
}

fn append_single_unit_action(sysdctl_args: &mut Vec<String>, parsed: &ParsedArgs) {
    sysdctl_args.push(parsed.command.clone());
    push_required_unit(sysdctl_args, &parsed.positional, &parsed.command);
}

fn append_unset_environment_args(sysdctl_args: &mut Vec<String>, parsed: ParsedArgs) {
    sysdctl_args.push("unset-environment".to_string());
    sysdctl_args.extend(parsed.positional);
}

fn append_optional_unit_action(sysdctl_args: &mut Vec<String>, parsed: ParsedArgs) {
    sysdctl_args.push(parsed.command.clone());
    push_optional_unit(sysdctl_args, &parsed.positional);
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
