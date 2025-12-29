//! Zombie process reaping for PID 1
//!
//! When a process's parent dies, it gets reparented to PID 1.
//! PID 1 must call wait() to clean up these orphaned zombies.

use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Information about a reaped process
#[derive(Debug, Clone)]
pub struct ReapedProcess {
    pub pid: i32,
    pub status: WaitResult,
}

/// Exit status of a reaped process
#[derive(Debug, Clone)]
pub enum WaitResult {
    /// Process exited normally with code
    Exited(i32),
    /// Process killed by signal
    Signaled(i32),
    /// Unknown status
    Unknown,
}

impl WaitResult {
    fn from_wait_status(status: WaitStatus) -> Self {
        match status {
            WaitStatus::Exited(_, code) => WaitResult::Exited(code),
            WaitStatus::Signaled(_, signal, _) => WaitResult::Signaled(signal as i32),
            _ => WaitResult::Unknown,
        }
    }
}

/// Zombie process reaper
///
/// Runs a background task that reaps all zombie processes.
/// Notifies interested parties when specific PIDs exit.
pub struct ZombieReaper {
    /// Channel to send reaped process info
    tx: mpsc::Sender<ReapedProcess>,
    /// Channel to receive reaped process info
    rx: Option<mpsc::Receiver<ReapedProcess>>,
    /// PIDs we're interested in tracking (service PIDs)
    watched_pids: HashMap<i32, String>,
}

impl ZombieReaper {
    /// Create a new zombie reaper
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(256);
        Self {
            tx,
            rx: Some(rx),
            watched_pids: HashMap::new(),
        }
    }

    /// Take the receiver for reaped processes
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<ReapedProcess>> {
        self.rx.take()
    }

    /// Watch a PID (associate with service name)
    pub fn watch(&mut self, pid: i32, name: String) {
        self.watched_pids.insert(pid, name);
    }

    /// Stop watching a PID
    pub fn unwatch(&mut self, pid: i32) {
        self.watched_pids.remove(&pid);
    }

    /// Get the service name for a watched PID
    pub fn get_service(&self, pid: i32) -> Option<&String> {
        self.watched_pids.get(&pid)
    }

    /// Reap all available zombie processes (non-blocking)
    ///
    /// Returns the number of processes reaped.
    pub fn reap_all(&self) -> usize {
        let mut count = 0;

        loop {
            // Wait for any child, non-blocking
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::StillAlive) => {
                    // No more zombies
                    break;
                }
                Ok(status) => {
                    let pid = match status {
                        WaitStatus::Exited(p, _) => p.as_raw(),
                        WaitStatus::Signaled(p, _, _) => p.as_raw(),
                        WaitStatus::Stopped(p, _) => p.as_raw(),
                        WaitStatus::Continued(p) => p.as_raw(),
                        _ => continue,
                    };

                    let result = WaitResult::from_wait_status(status);
                    log::debug!("Reaped PID {} ({:?})", pid, result);

                    // Notify if anyone is listening
                    let _ = self.tx.try_send(ReapedProcess { pid, status: result });

                    count += 1;
                }
                Err(nix::errno::Errno::ECHILD) => {
                    // No children at all
                    break;
                }
                Err(e) => {
                    log::error!("waitpid error: {}", e);
                    break;
                }
            }
        }

        count
    }
}

impl Default for ZombieReaper {
    fn default() -> Self {
        Self::new()
    }
}

/// Reap zombies once (synchronous, non-blocking)
///
/// This is a simple function for use outside the reaper struct.
/// Returns the number of zombies reaped.
pub fn reap_zombies() -> usize {
    let mut count = 0;

    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => break,
            Ok(status) => {
                let pid = match status {
                    WaitStatus::Exited(p, code) => {
                        log::debug!("Reaped PID {} (exited {})", p.as_raw(), code);
                        p.as_raw()
                    }
                    WaitStatus::Signaled(p, sig, _) => {
                        log::debug!("Reaped PID {} (killed by {})", p.as_raw(), sig);
                        p.as_raw()
                    }
                    _ => continue,
                };
                let _ = pid; // suppress unused warning
                count += 1;
            }
            Err(nix::errno::Errno::ECHILD) => break,
            Err(e) => {
                log::error!("waitpid error: {}", e);
                break;
            }
        }
    }

    count
}
