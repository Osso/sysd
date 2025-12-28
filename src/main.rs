use std::path::PathBuf;
use clap::Parser;

#[derive(Parser)]
#[command(name = "sysd")]
#[command(about = "Minimal systemd-compatible init system")]
struct Args {
    /// Parse and display a service file (for testing)
    #[arg(long)]
    parse: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if let Some(path) = args.parse {
        let svc = sysd::units::load_service(&path).await?;
        println!("Parsed: {}", svc.name);
        println!();
        println!("[Unit]");
        if let Some(desc) = &svc.unit.description {
            println!("  Description: {}", desc);
        }
        if !svc.unit.after.is_empty() {
            println!("  After: {:?}", svc.unit.after);
        }
        if !svc.unit.wants.is_empty() {
            println!("  Wants: {:?}", svc.unit.wants);
        }
        if !svc.unit.requires.is_empty() {
            println!("  Requires: {:?}", svc.unit.requires);
        }
        println!();
        println!("[Service]");
        println!("  Type: {:?}", svc.service.service_type);
        if !svc.service.exec_start.is_empty() {
            println!("  ExecStart: {:?}", svc.service.exec_start);
        }
        println!("  Restart: {:?}", svc.service.restart);
        println!("  RestartSec: {:?}", svc.service.restart_sec);
        if let Some(mem) = svc.service.memory_max {
            println!("  MemoryMax: {} bytes", mem);
        }
        if let Some(tasks) = svc.service.tasks_max {
            println!("  TasksMax: {}", tasks);
        }
        println!();
        println!("[Install]");
        if !svc.install.wanted_by.is_empty() {
            println!("  WantedBy: {:?}", svc.install.wanted_by);
        }
    } else {
        println!("Usage: sysd --parse /path/to/service.file");
    }

    Ok(())
}
