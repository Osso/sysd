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
}

fn main() {
    let args = Args::parse();
    let user_mode = args.user;

    // Parse is local-only
    if let Command::Parse { path } = args.command {
        parse_local(&path);
        return;
    }

    let request = match args.command {
        Command::List { user: list_user, unit_type } => Request::List { user: list_user || user_mode, unit_type },
        Command::Start { name } => Request::Start { name },
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
        Command::Parse { .. } => unreachable!(),
    };

    // Use appropriate socket path based on --user flag
    let sock_path = socket_path(user_mode);

    match Client::call(&sock_path, &request) {
        Ok(response) => print_response(response),
        Err(e) => {
            if e.to_string().contains("connect") || e.to_string().contains("No such file") {
                if user_mode {
                    eprintln!("sysdctl: user daemon not running");
                    eprintln!("  start with: sysd --user");
                } else {
                    eprintln!("sysdctl: daemon not running");
                    eprintln!("  start with: sudo sysd");
                }
            } else {
                eprintln!("sysdctl: {}", e);
            }
            std::process::exit(1);
        }
    }
}

fn print_response(response: Response) {
    match response {
        Response::Ok => {} // Silent success
        Response::Pong => println!("pong"),
        Response::Error(msg) => {
            eprintln!("error: {}", msg);
            std::process::exit(1);
        }
        Response::Units(units) => {
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
        Response::Status(unit) => {
            println!("● {}", unit.name);
            println!("     Type: {}", unit.unit_type);
            println!("    State: {}", unit.state);
            if let Some(desc) = unit.description {
                println!("    Desc:  {}", desc);
            }
        }
        Response::Deps(deps) => {
            if deps.is_empty() {
                println!("No dependencies");
            } else {
                for dep in deps {
                    println!("  {}", dep);
                }
            }
        }
        Response::BootTarget(target) => {
            println!("{}", target);
        }
        Response::BootPlan(units) => {
            if units.is_empty() {
                println!("Nothing to start");
            } else {
                for unit in units {
                    println!("  → {}", unit);
                }
            }
        }
        Response::EnabledState(state) => {
            println!("{}", state);
            // Exit with code 1 if disabled (like systemctl)
            if state == "disabled" {
                std::process::exit(1);
            }
        }
    }
}

fn parse_local(path: &PathBuf) {
    // Use tokio runtime for async parsing
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        match sysd::units::load_unit(path).await {
            Ok(unit) => {
                println!("Name: {}", unit.name());
                let section = unit.unit_section();
                if let Some(desc) = &section.description {
                    println!("Description: {}", desc);
                }
                if !section.after.is_empty() {
                    println!("After: {}", section.after.join(", "));
                }
                if !section.requires.is_empty() {
                    println!("Requires: {}", section.requires.join(", "));
                }
                if !section.wants.is_empty() {
                    println!("Wants: {}", section.wants.join(", "));
                }
            }
            Err(e) => {
                eprintln!("Failed to parse: {}", e);
                std::process::exit(1);
            }
        }
    });
}
