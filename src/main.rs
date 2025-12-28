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
            parse_and_display(&path).await?;
        }
    }

    Ok(())
}

/// Parse a service file and print its contents
async fn parse_and_display(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let svc = sysd::units::load_service(path).await?;

    println!("Parsed: {}", svc.name);
    println!();

    // [Unit] section
    println!("[Unit]");
    if let Some(desc) = &svc.unit.description {
        println!("  Description = {}", desc);
    }
    if !svc.unit.after.is_empty() {
        println!("  After = {}", svc.unit.after.join(" "));
    }
    if !svc.unit.before.is_empty() {
        println!("  Before = {}", svc.unit.before.join(" "));
    }
    if !svc.unit.wants.is_empty() {
        println!("  Wants = {}", svc.unit.wants.join(" "));
    }
    if !svc.unit.requires.is_empty() {
        println!("  Requires = {}", svc.unit.requires.join(" "));
    }
    if !svc.unit.conflicts.is_empty() {
        println!("  Conflicts = {}", svc.unit.conflicts.join(" "));
    }

    // [Service] section
    println!();
    println!("[Service]");
    println!("  Type = {:?}", svc.service.service_type);
    for cmd in &svc.service.exec_start_pre {
        println!("  ExecStartPre = {}", cmd);
    }
    for cmd in &svc.service.exec_start {
        println!("  ExecStart = {}", cmd);
    }
    for cmd in &svc.service.exec_start_post {
        println!("  ExecStartPost = {}", cmd);
    }
    for cmd in &svc.service.exec_stop {
        println!("  ExecStop = {}", cmd);
    }
    for cmd in &svc.service.exec_reload {
        println!("  ExecReload = {}", cmd);
    }
    println!("  Restart = {:?}", svc.service.restart);
    println!("  RestartSec = {:?}", svc.service.restart_sec);
    if let Some(t) = svc.service.timeout_start_sec {
        println!("  TimeoutStartSec = {:?}", t);
    }
    if let Some(t) = svc.service.timeout_stop_sec {
        println!("  TimeoutStopSec = {:?}", t);
    }
    if let Some(user) = &svc.service.user {
        println!("  User = {}", user);
    }
    if let Some(group) = &svc.service.group {
        println!("  Group = {}", group);
    }
    if let Some(wd) = &svc.service.working_directory {
        println!("  WorkingDirectory = {}", wd.display());
    }
    for (k, v) in &svc.service.environment {
        println!("  Environment = {}={}", k, v);
    }
    for f in &svc.service.environment_file {
        println!("  EnvironmentFile = {}", f.display());
    }
    if let Some(mem) = svc.service.memory_max {
        println!("  MemoryMax = {} bytes", mem);
    }
    if let Some(cpu) = svc.service.cpu_quota {
        println!("  CPUQuota = {}%", cpu);
    }
    if let Some(tasks) = svc.service.tasks_max {
        println!("  TasksMax = {}", tasks);
    }

    // [Install] section
    if !svc.install.wanted_by.is_empty() || !svc.install.required_by.is_empty() {
        println!();
        println!("[Install]");
        if !svc.install.wanted_by.is_empty() {
            println!("  WantedBy = {}", svc.install.wanted_by.join(" "));
        }
        if !svc.install.required_by.is_empty() {
            println!("  RequiredBy = {}", svc.install.required_by.join(" "));
        }
    }

    Ok(())
}
