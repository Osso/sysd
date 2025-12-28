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
    /// Parse a unit file and display its contents (for testing)
    Parse {
        /// Path to the .service file
        path: PathBuf,
    },
    // Future commands:
    // /// Run as PID 1 init system
    // Init,
    // /// Start the D-Bus service manager (non-PID 1 mode)
    // Daemon,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    match args.command {
        Command::Parse { path } => {
            commands::parse(&path).await?;
        }
    }

    Ok(())
}
