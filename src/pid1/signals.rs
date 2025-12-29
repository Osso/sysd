//! Signal handling for PID 1
//!
//! PID 1 has special signal handling requirements:
//! - SIGTERM/SIGINT: Initiate shutdown
//! - SIGCHLD: Reap zombie processes
//! - SIGUSR1/SIGUSR2: Custom actions (e.g., debug, reload)

use tokio::signal::unix::{signal, Signal, SignalKind};
use tokio::sync::mpsc;

/// Signals that sysd handles
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysdSignal {
    /// Child process exited (SIGCHLD)
    Child,
    /// Shutdown request (SIGTERM)
    Term,
    /// Interrupt (SIGINT, Ctrl+C)
    Int,
    /// Hangup (SIGHUP) - reload config
    Hup,
    /// User signal 1 (SIGUSR1) - debug dump
    Usr1,
}

/// Signal handler for PID 1
pub struct SignalHandler {
    sigchld: Signal,
    sigterm: Signal,
    sigint: Signal,
    sighup: Signal,
    sigusr1: Signal,
}

impl SignalHandler {
    /// Create a new signal handler
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            sigchld: signal(SignalKind::child())?,
            sigterm: signal(SignalKind::terminate())?,
            sigint: signal(SignalKind::interrupt())?,
            sighup: signal(SignalKind::hangup())?,
            sigusr1: signal(SignalKind::user_defined1())?,
        })
    }

    /// Wait for the next signal
    pub async fn wait(&mut self) -> SysdSignal {
        tokio::select! {
            _ = self.sigchld.recv() => SysdSignal::Child,
            _ = self.sigterm.recv() => SysdSignal::Term,
            _ = self.sigint.recv() => SysdSignal::Int,
            _ = self.sighup.recv() => SysdSignal::Hup,
            _ = self.sigusr1.recv() => SysdSignal::Usr1,
        }
    }

    /// Spawn a task that forwards signals to a channel
    pub fn spawn_forwarder(mut self) -> mpsc::Receiver<SysdSignal> {
        let (tx, rx) = mpsc::channel(32);

        tokio::spawn(async move {
            loop {
                let sig = self.wait().await;
                if tx.send(sig).await.is_err() {
                    // Receiver dropped, exit
                    break;
                }
            }
        });

        rx
    }
}
