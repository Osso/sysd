//! sysdctl - CLI for sysd
//!
//! Communicates with the sysd daemon over /run/sysd.sock.
//! Use --user to communicate with the user service manager.

use clap::{Parser, Subcommand};
use peercred_ipc::Client;
use std::path::PathBuf;
use sysd::protocol::{socket_path, Request, Response};

#[derive(Parser)]
#[command(name = "sysdctl")]
#[command(about = "Control the sysd init system")]
struct Args {
    /// Connect to user service manager instead of system
    #[arg(long, global = true)]
    user: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List loaded units
    List {
        /// Show user units instead of system units
        #[arg(long)]
        user: bool,
        /// Filter by unit type (service, socket, mount, slice, target)
        #[arg(short = 't', long = "type")]
        unit_type: Option<String>,
    },

    /// Start a unit
    Start {
        /// Unit name (e.g., "docker" or "docker.service")
        name: String,
        /// Wait for the unit to exit (become inactive or failed)
        #[arg(long)]
        wait: bool,
        /// Job mode (fail, replace, replace-irreversibly, isolate, ignore-dependencies)
        #[arg(long, default_value = "replace")]
        job_mode: String,
    },

    /// Stop a unit
    Stop {
        /// Unit name
        name: String,
    },

    /// Restart a unit
    Restart {
        /// Unit name
        name: String,
    },

    /// Enable a unit to start at boot
    Enable {
        /// Unit name
        name: String,
    },

    /// Disable a unit from starting at boot
    Disable {
        /// Unit name
        name: String,
    },

    /// Check if a unit is enabled
    IsEnabled {
        /// Unit name
        name: String,
    },

    /// Show unit status
    Status {
        /// Unit name
        name: String,
    },

    /// Show unit dependencies
    Deps {
        /// Unit name
        name: String,
    },

    /// Show the default boot target
    GetBootTarget,

    /// Reload unit files from disk
    Reload,

    /// Sync units (reload + restart changed)
    Sync,

    /// Switch to a target (stop unrelated units)
    SwitchTarget {
        /// Target name (e.g., "multi-user.target")
        target: String,
    },

    /// Parse a unit file locally (doesn't require daemon)
    Parse {
        /// Path to the unit file
        path: PathBuf,
    },

    /// Ping the daemon
    Ping,

    /// Import environment variables to the service manager
    ImportEnvironment,

    /// Unset environment variables in the service manager
    UnsetEnvironment {
        /// Variable names to unset
        names: Vec<String>,
    },

    /// Reset failed state of all units
    ResetFailed,

    /// Check if a unit is active (exit 0 if active, 3 if inactive/failed)
    IsActive {
        /// Unit name
        name: String,
        /// Quiet mode - no output, just exit code
        #[arg(short, long)]
        quiet: bool,
    },
}

fn main() {
    let args = Args::parse();
    let user_mode = args.user;

    if let Command::Parse { path } = args.command {
        parse_local(&path);
        return;
    }

    let Some(request) = build_request_or_exit(args.command, user_mode) else {
        return;
    };

    send_request_or_exit(user_mode, request);
}

fn build_request_or_exit(command: Command, user_mode: bool) -> Option<Request> {
    match command {
        Command::IsActive { name, quiet } => handle_is_active_or_exit(user_mode, name, quiet),
        Command::Parse { .. } => unreachable!(),
        command => Some(build_regular_request(command, user_mode)),
    }
}

fn build_regular_request(command: Command, user_mode: bool) -> Request {
    match command {
        Command::List {
            user: list_user,
            unit_type,
        } => Request::List {
            user: list_user || user_mode,
            unit_type,
        },
        Command::Start {
            name,
            wait,
            job_mode,
        } => start_request(name, wait, &job_mode),
        Command::Stop { name } => Request::Stop { name },
        Command::Restart { name } => Request::Restart { name },
        Command::Enable { name } => Request::Enable { name },
        Command::Disable { name } => Request::Disable { name },
        Command::IsEnabled { name } => Request::IsEnabled { name },
        Command::Status { name } => Request::Status { name },
        Command::Deps { name } => Request::Deps { name },
        Command::GetBootTarget => Request::GetBootTarget,
        Command::Reload => Request::ReloadUnitFiles,
        Command::Sync => Request::SyncUnits,
        Command::SwitchTarget { target } => Request::SwitchTarget { target },
        Command::Ping => Request::Ping,
        Command::ImportEnvironment => Request::ImportEnvironment {
            vars: std::env::vars().collect(),
        },
        Command::UnsetEnvironment { names } => Request::UnsetEnvironment { names },
        Command::ResetFailed => Request::ResetFailed,
        Command::IsActive { .. } | Command::Parse { .. } => unreachable!(),
    }
}

fn start_request(name: String, wait: bool, job_mode: &str) -> Request {
    if job_mode != "replace" && job_mode != "fail" {
        log::debug!("job_mode={} (treated as replace)", job_mode);
    }
    if wait {
        Request::StartAndWait { name }
    } else {
        Request::Start { name }
    }
}

fn handle_is_active_or_exit(user_mode: bool, name: String, quiet: bool) -> Option<Request> {
    let sock_path = socket_path(user_mode);
    let result = Client::call(&sock_path, &Request::IsActive { name: name.clone() });
    match result {
        Ok(Response::ActiveState(state)) => {
            if !quiet {
                println!("{}", state);
            }
            if state == "active" || state == "Active" {
                std::process::exit(0);
            }
            std::process::exit(3);
        }
        Ok(Response::Error(msg)) => {
            if !quiet {
                eprintln!("error: {}", msg);
            }
            std::process::exit(3);
        }
        Ok(_) => {
            if !quiet {
                eprintln!("unexpected response");
            }
            std::process::exit(3);
        }
        Err(error) => {
            if !quiet {
                eprintln!("sysdctl: {}", error);
            }
            std::process::exit(1);
        }
    }
}

fn send_request_or_exit(user_mode: bool, request: Request) {
    let sock_path = socket_path(user_mode);
    match Client::call(&sock_path, &request) {
        Ok(response) => print_response(response),
        Err(error) => handle_daemon_error(user_mode, &error.to_string()),
    }
}

fn handle_daemon_error(user_mode: bool, message: &str) {
    if message.contains("connect") || message.contains("No such file") {
        if user_mode {
            eprintln!("sysdctl: user daemon not running");
            eprintln!("  start with: sysd --user");
        } else {
            eprintln!("sysdctl: daemon not running");
            eprintln!("  start with: sudo sysd");
        }
    } else {
        eprintln!("sysdctl: {}", message);
    }
    std::process::exit(1);
}

fn print_response(response: Response) {
    match response {
        Response::Ok => {} // Silent success
        Response::Pong => println!("pong"),
        Response::Error(msg) => print_error_and_exit(&msg),
        Response::Units(units) => print_units(units),
        Response::Status(unit) => print_status(unit),
        Response::Deps(deps) => print_deps(deps),
        Response::BootTarget(target) => println!("{}", target),
        Response::BootPlan(units) => print_boot_plan(units),
        Response::EnabledState(state) => print_enabled_state(&state),
        Response::ActiveState(state) => print_active_state(&state),
    }
}

fn print_error_and_exit(message: &str) {
    eprintln!("error: {}", message);
    std::process::exit(1);
}

fn print_units(units: Vec<sysd::protocol::UnitInfo>) {
    if units.is_empty() {
        println!("No units loaded");
        return;
    }
    println!("{:<40} {:>10} {:>12}", "UNIT", "TYPE", "STATE");
    for unit in units {
        println!(
            "{:<40} {:>10} {:>12}",
            unit.name, unit.unit_type, unit.state
        );
    }
}

fn print_status(unit: sysd::protocol::UnitInfo) {
    println!("● {}", unit.name);
    println!("     Type: {}", unit.unit_type);
    println!("    State: {}", unit.state);
    if let Some(desc) = unit.description {
        println!("    Desc:  {}", desc);
    }
}

fn print_deps(deps: Vec<String>) {
    if deps.is_empty() {
        println!("No dependencies");
        return;
    }
    for dep in deps {
        println!("  {}", dep);
    }
}

fn print_boot_plan(units: Vec<String>) {
    if units.is_empty() {
        println!("Nothing to start");
        return;
    }
    for unit in units {
        println!("  → {}", unit);
    }
}

fn print_enabled_state(state: &str) {
    println!("{}", state);
    if state == "disabled" {
        std::process::exit(1);
    }
}

fn print_active_state(state: &str) {
    println!("{}", state);
    if state != "active" {
        std::process::exit(3);
    }
}

fn parse_local(path: &PathBuf) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let result = rt.block_on(sysd::units::load_unit(path));
    match result {
        Ok(unit) => print_parsed_unit(&unit),
        Err(error) => {
            eprintln!("Failed to parse: {}", error);
            std::process::exit(1);
        }
    }
}

fn print_parsed_unit(unit: &sysd::units::Unit) {
    println!("Name: {}", unit.name());
    let section = unit.unit_section();
    if let Some(desc) = &section.description {
        println!("Description: {}", desc);
    }
    print_non_empty("After", &section.after);
    print_non_empty("Requires", &section.requires);
    print_non_empty("Wants", &section.wants);
}

fn print_non_empty(label: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    println!("{}: {}", label, values.join(", "));
}
