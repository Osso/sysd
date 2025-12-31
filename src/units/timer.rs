//! Timer unit type for scheduled activation
//!
//! Parses .timer unit files and manages time-based service activation.

use super::{InstallSection, UnitSection};
use std::time::Duration;

/// Calendar event specification for OnCalendar=
#[derive(Debug, Clone, PartialEq)]
pub enum CalendarSpec {
    /// Named shortcuts: minutely, hourly, daily, weekly, monthly, yearly
    Named(String),
    /// Day of week: Mon, Tue, Wed, Thu, Fri, Sat, Sun
    DayOfWeek(String),
    /// Time only: HH:MM or HH:MM:SS (runs daily at that time)
    Time { hour: u32, minute: u32, second: u32 },
    /// Full calendar spec: *-*-* HH:MM:SS or similar
    Full(String),
}

impl CalendarSpec {
    /// Parse a calendar specification string
    pub fn parse(s: &str) -> Self {
        let s = s.trim();

        // Named shortcuts
        match s.to_lowercase().as_str() {
            "minutely" | "hourly" | "daily" | "weekly" | "monthly" | "yearly" | "quarterly"
            | "semiannually" | "annually" => {
                return CalendarSpec::Named(s.to_lowercase());
            }
            _ => {}
        }

        // Day of week only
        match s.to_lowercase().as_str() {
            "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun" | "monday" | "tuesday"
            | "wednesday" | "thursday" | "friday" | "saturday" | "sunday" => {
                return CalendarSpec::DayOfWeek(s.to_string());
            }
            _ => {}
        }

        // Time only (HH:MM or HH:MM:SS)
        if !s.contains('-') && !s.contains('*') && s.contains(':') {
            let parts: Vec<&str> = s.split(':').collect();
            if parts.len() >= 2 {
                if let (Ok(hour), Ok(minute)) = (parts[0].parse(), parts[1].parse()) {
                    let second = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                    return CalendarSpec::Time {
                        hour,
                        minute,
                        second,
                    };
                }
            }
        }

        // Full calendar expression - store as-is for now
        CalendarSpec::Full(s.to_string())
    }

    /// Check if this is a daily schedule at a specific time
    pub fn is_daily(&self) -> bool {
        matches!(self, CalendarSpec::Named(s) if s == "daily") || matches!(self, CalendarSpec::Time { .. })
    }

    /// Check if this is a weekly schedule
    pub fn is_weekly(&self) -> bool {
        matches!(self, CalendarSpec::Named(s) if s == "weekly")
            || matches!(self, CalendarSpec::DayOfWeek(_))
    }
}

/// Timer section configuration
#[derive(Debug, Clone, Default)]
pub struct TimerSection {
    /// Calendar-based timer (OnCalendar=)
    pub on_calendar: Vec<CalendarSpec>,

    /// Time after boot (OnBootSec=)
    pub on_boot_sec: Option<Duration>,

    /// Time after last activation of this timer (OnActiveSec=)
    pub on_active_sec: Option<Duration>,

    /// Time after systemd startup (OnStartupSec=)
    pub on_startup_sec: Option<Duration>,

    /// Time after the activated unit was last activated (OnUnitActiveSec=)
    pub on_unit_active_sec: Option<Duration>,

    /// Time after the activated unit was last deactivated (OnUnitInactiveSec=)
    pub on_unit_inactive_sec: Option<Duration>,

    /// Coalescing accuracy window (AccuracySec=)
    pub accuracy_sec: Duration,

    /// Add random delay (RandomizedDelaySec=)
    pub randomized_delay_sec: Option<Duration>,

    /// Persist timer across reboots (Persistent=)
    pub persistent: bool,

    /// Wake system from suspend (WakeSystem=)
    pub wake_system: bool,

    /// Only trigger when system is not on battery (OnClockChange=, OnTimezoneChange=)
    pub on_clock_change: bool,
    pub on_timezone_change: bool,

    /// Unit to activate (Unit=, defaults to same name with .service)
    pub unit: Option<String>,
}

impl TimerSection {
    pub fn new() -> Self {
        Self {
            // Default accuracy is 1 minute
            accuracy_sec: Duration::from_secs(60),
            ..Default::default()
        }
    }
}

/// Represents a parsed .timer unit file
#[derive(Debug, Clone)]
pub struct Timer {
    /// Unit name (e.g., "fstrim.timer")
    pub name: String,
    /// [Unit] section
    pub unit: UnitSection,
    /// [Timer] section
    pub timer: TimerSection,
    /// [Install] section
    pub install: InstallSection,
}

impl Timer {
    pub fn new(name: String) -> Self {
        Self {
            name,
            unit: UnitSection::default(),
            timer: TimerSection::new(),
            install: InstallSection::default(),
        }
    }

    /// Get the service name this timer activates
    pub fn service_name(&self) -> String {
        if let Some(ref unit) = self.timer.unit {
            unit.clone()
        } else {
            // Default: same name with .service extension
            self.name.replace(".timer", ".service")
        }
    }

    /// Check if this is a monotonic timer (boot/startup/active based)
    pub fn is_monotonic(&self) -> bool {
        self.timer.on_boot_sec.is_some()
            || self.timer.on_startup_sec.is_some()
            || self.timer.on_active_sec.is_some()
            || self.timer.on_unit_active_sec.is_some()
            || self.timer.on_unit_inactive_sec.is_some()
    }

    /// Check if this is a realtime/calendar timer
    pub fn is_realtime(&self) -> bool {
        !self.timer.on_calendar.is_empty()
    }
}
