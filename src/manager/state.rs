//! Service state machine
//!
//! ```text
//!     ┌──────────┐
//!     │ Inactive │
//!     └────┬─────┘
//!          │ start
//!     ┌────▼─────┐
//!     │ Starting │──────────────┐
//!     └────┬─────┘              │ timeout/fail
//!          │ ready              │
//!     ┌────▼─────┐         ┌────▼────┐
//!     │ Running  │         │ Failed  │
//!     └────┬─────┘         └─────────┘
//!          │ stop/exit
//!     ┌────▼─────┐
//!     │ Stopping │
//!     └────┬─────┘
//!          │ exited
//!     ┌────▼─────┐
//!     │ Inactive │ (or restart)
//!     └──────────┘
//! ```

use std::time::Instant;

/// High-level service state (maps to systemd's ActiveState)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveState {
    Inactive,
    Activating,
    Active,
    Deactivating,
    Failed,
}

impl ActiveState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Activating => "activating",
            Self::Active => "active",
            Self::Deactivating => "deactivating",
            Self::Failed => "failed",
        }
    }
}

/// Detailed service state (maps to systemd's SubState)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubState {
    Dead,
    Starting,
    Running,
    Stopping,
    Failed,
    Exited,
    AutoRestart,  // Waiting for restart delay
}

impl SubState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Dead => "dead",
            Self::Starting => "start",
            Self::Running => "running",
            Self::Stopping => "stop",
            Self::Failed => "failed",
            Self::Exited => "exited",
            Self::AutoRestart => "auto-restart",
        }
    }
}

/// Runtime state of a service
#[derive(Debug)]
pub struct ServiceState {
    pub active: ActiveState,
    pub sub: SubState,
    /// Main process PID (if running)
    pub main_pid: Option<u32>,
    /// When the service entered current state
    pub state_change_time: Instant,
    /// Exit code of last run (if exited)
    pub exit_code: Option<i32>,
    /// Error message if failed
    pub error: Option<String>,
    /// When to restart (if auto-restart pending)
    pub restart_at: Option<Instant>,
    /// Number of restarts since last successful run
    pub restart_count: u32,
}

impl Default for ServiceState {
    fn default() -> Self {
        Self {
            active: ActiveState::Inactive,
            sub: SubState::Dead,
            main_pid: None,
            state_change_time: Instant::now(),
            exit_code: None,
            error: None,
            restart_at: None,
            restart_count: 0,
        }
    }
}

impl ServiceState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_starting(&mut self) {
        self.active = ActiveState::Activating;
        self.sub = SubState::Starting;
        self.state_change_time = Instant::now();
        self.exit_code = None;
        self.error = None;
    }

    pub fn set_running(&mut self, pid: u32) {
        self.active = ActiveState::Active;
        self.sub = SubState::Running;
        self.main_pid = Some(pid);
        self.state_change_time = Instant::now();
    }

    pub fn set_stopping(&mut self) {
        self.active = ActiveState::Deactivating;
        self.sub = SubState::Stopping;
        self.state_change_time = Instant::now();
    }

    pub fn set_stopped(&mut self, exit_code: i32) {
        self.active = ActiveState::Inactive;
        self.sub = if exit_code == 0 { SubState::Exited } else { SubState::Dead };
        self.main_pid = None;
        self.exit_code = Some(exit_code);
        self.state_change_time = Instant::now();
        self.restart_at = None;
    }

    /// Schedule an automatic restart after a delay
    pub fn set_auto_restart(&mut self, delay: std::time::Duration) {
        self.active = ActiveState::Activating;
        self.sub = SubState::AutoRestart;
        self.main_pid = None;
        self.restart_at = Some(Instant::now() + delay);
        self.restart_count += 1;
        self.state_change_time = Instant::now();
    }

    /// Check if restart is due
    pub fn restart_due(&self) -> bool {
        self.restart_at.map(|t| Instant::now() >= t).unwrap_or(false)
    }

    /// Clear restart state (after successful start)
    pub fn clear_restart(&mut self) {
        self.restart_at = None;
        // Don't reset count here - only on explicit stop or long uptime
    }

    /// Reset restart count (after successful long run)
    pub fn reset_restart_count(&mut self) {
        self.restart_count = 0;
    }

    pub fn set_failed(&mut self, error: String) {
        self.active = ActiveState::Failed;
        self.sub = SubState::Failed;
        self.main_pid = None;
        self.error = Some(error);
        self.state_change_time = Instant::now();
    }

    pub fn is_active(&self) -> bool {
        matches!(self.active, ActiveState::Active | ActiveState::Activating)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_new() {
        let state = ServiceState::new();
        assert_eq!(state.active, ActiveState::Inactive);
        assert_eq!(state.sub, SubState::Dead);
        assert!(state.main_pid.is_none());
        assert!(!state.is_active());
    }

    #[test]
    fn test_state_starting() {
        let mut state = ServiceState::new();
        state.set_starting();
        assert_eq!(state.active, ActiveState::Activating);
        assert_eq!(state.sub, SubState::Starting);
        assert!(state.is_active());
    }

    #[test]
    fn test_state_running() {
        let mut state = ServiceState::new();
        state.set_starting();
        state.set_running(1234);
        assert_eq!(state.active, ActiveState::Active);
        assert_eq!(state.sub, SubState::Running);
        assert_eq!(state.main_pid, Some(1234));
        assert!(state.is_active());
    }

    #[test]
    fn test_state_stopping() {
        let mut state = ServiceState::new();
        state.set_running(1234);
        state.set_stopping();
        assert_eq!(state.active, ActiveState::Deactivating);
        assert_eq!(state.sub, SubState::Stopping);
        assert!(!state.is_active());
    }

    #[test]
    fn test_state_stopped_clean() {
        let mut state = ServiceState::new();
        state.set_running(1234);
        state.set_stopped(0);
        assert_eq!(state.active, ActiveState::Inactive);
        assert_eq!(state.sub, SubState::Exited);
        assert!(state.main_pid.is_none());
        assert_eq!(state.exit_code, Some(0));
    }

    #[test]
    fn test_state_stopped_error() {
        let mut state = ServiceState::new();
        state.set_running(1234);
        state.set_stopped(1);
        assert_eq!(state.active, ActiveState::Inactive);
        assert_eq!(state.sub, SubState::Dead);
        assert_eq!(state.exit_code, Some(1));
    }

    #[test]
    fn test_state_failed() {
        let mut state = ServiceState::new();
        state.set_running(1234);
        state.set_failed("timeout".to_string());
        assert_eq!(state.active, ActiveState::Failed);
        assert_eq!(state.sub, SubState::Failed);
        assert!(state.main_pid.is_none());
        assert_eq!(state.error, Some("timeout".to_string()));
    }

    #[test]
    fn test_active_state_as_str() {
        assert_eq!(ActiveState::Inactive.as_str(), "inactive");
        assert_eq!(ActiveState::Activating.as_str(), "activating");
        assert_eq!(ActiveState::Active.as_str(), "active");
        assert_eq!(ActiveState::Deactivating.as_str(), "deactivating");
        assert_eq!(ActiveState::Failed.as_str(), "failed");
    }

    #[test]
    fn test_sub_state_as_str() {
        assert_eq!(SubState::Dead.as_str(), "dead");
        assert_eq!(SubState::Starting.as_str(), "start");
        assert_eq!(SubState::Running.as_str(), "running");
        assert_eq!(SubState::Stopping.as_str(), "stop");
        assert_eq!(SubState::Failed.as_str(), "failed");
        assert_eq!(SubState::Exited.as_str(), "exited");
    }
}
