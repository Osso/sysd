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
