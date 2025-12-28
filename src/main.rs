mod commands;

use std::path::PathBuf;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "sysd")]
#[command(about = "Minimal systemd-compatible init system")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List available units
    List {
        /// Show user units instead of system units
        #[arg(long)]
        user: bool,
    },

    /// Parse a unit file and display its contents
    Parse {
        /// Path to the .service file
        path: PathBuf,
    },

    /// Start a service
    Start {
        /// Service name (e.g., "docker" or "docker.service")
        name: String,
    },

    /// Stop a service
    Stop {
        /// Service name
        name: String,
    },

    /// Show service status
    Status {
        /// Service name
        name: String,
    },

    /// Show service dependencies
    Deps {
        /// Service name
        name: String,
    },

    /// Show the default boot target
    GetBootTarget,

    /// Boot to default target (start all services)
    Boot {
        /// Show what would be started without actually starting
        #[arg(long, short = 'n')]
        dry_run: bool,
    },

    /// Reload unit files from disk (like systemctl daemon-reload)
    ReloadUnitFiles,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info")
    ).init();

    let args = Args::parse();

    match args.command {
        Command::List { user } => {
            commands::list(user).await?;
        }
        Command::Parse { path } => {
            commands::parse(&path).await?;
        }
        Command::Start { name } => {
            commands::start(&name).await?;
        }
        Command::Stop { name } => {
            commands::stop(&name).await?;
        }
        Command::Status { name } => {
            commands::status(&name).await?;
        }
        Command::Deps { name } => {
            commands::deps(&name).await?;
        }
        Command::GetBootTarget => {
            commands::default_target().await?;
        }
        Command::Boot { dry_run } => {
            commands::boot(dry_run).await?;
        }
        Command::ReloadUnitFiles => {
            commands::reload_unit_files().await?;
        }
    }

    Ok(())
}
