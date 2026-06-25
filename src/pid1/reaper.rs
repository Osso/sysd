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
                    let _ = self.tx.try_send(ReapedProcess {
                        pid,
                        status: result,
                    });

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

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::signal::Signal;

    #[test]
    fn wait_result_decodes_exit_signal_and_unknown_statuses() {
        assert!(matches!(
            WaitResult::from_wait_status(WaitStatus::Exited(Pid::from_raw(42), 7)),
            WaitResult::Exited(7)
        ));
        assert!(matches!(
            WaitResult::from_wait_status(WaitStatus::Signaled(
                Pid::from_raw(42),
                Signal::SIGTERM,
                false,
            )),
            WaitResult::Signaled(signal) if signal == Signal::SIGTERM as i32
        ));
        assert!(matches!(
            WaitResult::from_wait_status(WaitStatus::StillAlive),
            WaitResult::Unknown
        ));
    }

    #[test]
    fn reaper_tracks_watched_services_and_receiver_ownership() {
        let mut reaper = ZombieReaper::new();

        assert!(reaper.take_receiver().is_some());
        assert!(reaper.take_receiver().is_none());
        reaper.watch(1234, "demo.service".to_string());
        assert_eq!(
            reaper.get_service(1234).map(String::as_str),
            Some("demo.service")
        );
        reaper.unwatch(1234);
        assert!(reaper.get_service(1234).is_none());
        let _ = ZombieReaper::default();
    }
}
