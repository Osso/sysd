// Timer unit operations
//
// Handles scheduling and activation of timer units.

use tokio::sync::mpsc;

use crate::units::Timer;

use super::{timer_scheduler, Manager, ManagerError};

impl Manager {
    /// Start a timer unit (schedule service activation)
    pub(super) async fn start_timer(
        &mut self,
        name: &str,
        timer: &Timer,
    ) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if state.is_active() {
            return Err(ManagerError::AlreadyActive(name.to_string()));
        }

        state.set_starting();

        log::info!("Starting timer {}", name);

        // Calculate next trigger time
        let next_trigger = timer_scheduler::calculate_next_trigger(timer, self.boot_time);

        if let Some(delay) = next_trigger {
            let service_name = timer.service_name();
            let timer_name = name.to_string();
            let tx = self.timer_tx.clone();

            log::debug!("{}: scheduling to fire in {:?}", name, delay);

            // Spawn timer watcher task
            tokio::spawn(async move {
                timer_scheduler::watch_timer(timer_name, service_name, delay, tx).await;
            });
        } else {
            log::debug!("{}: no trigger configured, timer idle", name);
        }

        // Mark as active
        if let Some(state) = self.states.get_mut(name) {
            state.set_running(0);
        }

        log::info!("{} active", name);
        Ok(())
    }

    /// Stop a timer unit
    pub(super) async fn stop_timer(&mut self, name: &str) -> Result<(), ManagerError> {
        let state = self
            .states
            .get_mut(name)
            .ok_or_else(|| ManagerError::NotFound(name.to_string()))?;

        if !state.is_active() {
            return Err(ManagerError::NotActive(name.to_string()));
        }

        state.set_stopping();

        log::info!("Stopping timer {}", name);

        // Timer tasks will complete naturally or on next fire
        // For now, we just mark the timer as stopped

        if let Some(state) = self.states.get_mut(name) {
            state.set_stopped(0);
        }

        log::info!("{} stopped", name);
        Ok(())
    }

    /// Take the timer fired receiver (for use in event loops)
    pub fn take_timer_rx(&mut self) -> Option<mpsc::Receiver<timer_scheduler::TimerFired>> {
        self.timer_rx.take()
    }

    /// Process a timer fired message (start the associated service)
    pub async fn handle_timer_fired(
        &mut self,
        fired: timer_scheduler::TimerFired,
    ) -> Result<(), ManagerError> {
        log::info!(
            "Timer fired: {} triggered by {}",
            fired.service_name,
            fired.timer_name
        );

        // Check if service is already running
        if let Some(state) = self.states.get(&fired.service_name) {
            if state.is_active() {
                log::debug!(
                    "{} already running, skipping timer activation",
                    fired.service_name
                );
                // Reschedule the timer for next trigger
                self.reschedule_timer(&fired.timer_name).await;
                return Ok(());
            }
        }

        // Start the service
        let result = self.start(&fired.service_name).await;

        // Reschedule the timer for next trigger (for repeating timers)
        self.reschedule_timer(&fired.timer_name).await;

        result
    }

    /// Reschedule a timer after it fires
    async fn reschedule_timer(&mut self, timer_name: &str) {
        let Some(unit) = self.units.get(timer_name).cloned() else {
            return;
        };
        let Some(timer) = unit.as_timer() else {
            return;
        };
        if !timer_repeats(timer) {
            return;
        }
        let Some(delay) = timer_scheduler::calculate_next_trigger(timer, self.boot_time) else {
            return;
        };
        schedule_timer_watch(timer_name, timer, delay, self.timer_tx.clone());
    }
}

fn timer_repeats(timer: &Timer) -> bool {
    timer.timer.on_unit_active_sec.is_some() || !timer.timer.on_calendar.is_empty()
}

fn schedule_timer_watch(
    timer_name: &str,
    timer: &Timer,
    delay: std::time::Duration,
    tx: mpsc::Sender<timer_scheduler::TimerFired>,
) {
    let service_name = timer.service_name();
    let timer_name = timer_name.to_string();
    log::debug!("{}: rescheduling to fire in {:?}", timer_name, delay);
    tokio::spawn(async move {
        timer_scheduler::watch_timer(timer_name, service_name, delay, tx).await;
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::manager::{ActiveState, ServiceState};
    use crate::units::{Service, Unit};

    fn manager_with_timer(name: &str, timer: Timer) -> Manager {
        let mut manager = Manager::new();
        manager
            .units
            .insert(name.to_string(), Unit::Timer(timer));
        manager
            .states
            .insert(name.to_string(), ServiceState::new());
        manager
    }

    fn timer_named(name: &str) -> Timer {
        Timer::new(name.to_string())
    }

    #[tokio::test]
    async fn start_timer_reports_missing_and_already_active_states() {
        let mut manager = Manager::new();
        let timer = timer_named("demo.timer");
        let missing = manager.start_timer("demo.timer", &timer).await.unwrap_err();
        assert!(matches!(missing, ManagerError::NotFound(name) if name == "demo.timer"));

        manager
            .states
            .insert("demo.timer".to_string(), ServiceState::new());
        manager
            .states
            .get_mut("demo.timer")
            .unwrap()
            .set_running(0);
        let active = manager.start_timer("demo.timer", &timer).await.unwrap_err();
        assert!(matches!(active, ManagerError::AlreadyActive(name) if name == "demo.timer"));
    }

    #[tokio::test]
    async fn start_timer_without_trigger_still_marks_timer_active() {
        let timer = timer_named("idle.timer");
        let mut manager = manager_with_timer("idle.timer", timer.clone());

        manager.start_timer("idle.timer", &timer).await.unwrap();

        let state = manager.states.get("idle.timer").unwrap();
        assert_eq!(state.active, ActiveState::Active);
        assert!(state.is_active());
    }

    #[tokio::test]
    async fn stop_timer_requires_active_state_and_marks_timer_stopped() {
        let timer = timer_named("cleanup.timer");
        let mut manager = manager_with_timer("cleanup.timer", timer);

        let inactive = manager.stop_timer("cleanup.timer").await.unwrap_err();
        assert!(matches!(inactive, ManagerError::NotActive(name) if name == "cleanup.timer"));

        manager
            .states
            .get_mut("cleanup.timer")
            .unwrap()
            .set_running(0);
        manager.stop_timer("cleanup.timer").await.unwrap();

        let state = manager.states.get("cleanup.timer").unwrap();
        assert_eq!(state.active, ActiveState::Inactive);
    }

    #[tokio::test]
    async fn take_timer_rx_transfers_receiver_only_once() {
        let mut manager = Manager::new();

        assert!(manager.take_timer_rx().is_some());
        assert!(manager.take_timer_rx().is_none());
    }

    #[tokio::test]
    async fn handle_timer_fired_skips_service_that_is_already_active() {
        let mut timer = timer_named("repeat.timer");
        timer.timer.on_unit_active_sec = Some(Duration::ZERO);
        let mut manager = manager_with_timer("repeat.timer", timer);
        manager.units.insert(
            "repeat.service".to_string(),
            Unit::Service(Service::new("repeat.service".to_string())),
        );
        manager
            .states
            .insert("repeat.service".to_string(), ServiceState::new());
        manager
            .states
            .get_mut("repeat.service")
            .unwrap()
            .set_running(123);

        manager
            .handle_timer_fired(timer_scheduler::TimerFired {
                timer_name: "repeat.timer".to_string(),
                service_name: "repeat.service".to_string(),
            })
            .await
            .unwrap();

        assert_eq!(
            manager.states.get("repeat.service").unwrap().main_pid,
            Some(123)
        );
    }

    #[tokio::test]
    async fn handle_timer_fired_reports_missing_service() {
        let mut manager = Manager::new();

        let error = manager
            .handle_timer_fired(timer_scheduler::TimerFired {
                timer_name: "missing.timer".to_string(),
                service_name: "missing.service".to_string(),
            })
            .await
            .unwrap_err();

        assert!(matches!(error, ManagerError::NotFound(name) if name == "missing.service"));
    }

    #[test]
    fn timer_repeats_only_for_unit_active_or_calendar_timers() {
        let mut boot_only = timer_named("boot.timer");
        boot_only.timer.on_boot_sec = Some(Duration::from_secs(1));
        assert!(!timer_repeats(&boot_only));

        let mut active = timer_named("active.timer");
        active.timer.on_unit_active_sec = Some(Duration::from_secs(1));
        assert!(timer_repeats(&active));

        let mut calendar = timer_named("calendar.timer");
        calendar
            .timer
            .on_calendar
            .push(crate::units::CalendarSpec::Named("daily".to_string()));
        assert!(timer_repeats(&calendar));
    }
}
