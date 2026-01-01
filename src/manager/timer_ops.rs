//! Timer unit operations
//!
//! Handles scheduling and activation of timer units.

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
        if let Some(unit) = self.units.get(timer_name).cloned() {
            if let Some(timer) = unit.as_timer() {
                // Check for repeating timer conditions (OnUnitActiveSec, OnCalendar)
                let should_repeat = timer.timer.on_unit_active_sec.is_some()
                    || !timer.timer.on_calendar.is_empty();

                if should_repeat {
                    if let Some(delay) =
                        timer_scheduler::calculate_next_trigger(timer, self.boot_time)
                    {
                        let service_name = timer.service_name();
                        let timer_name = timer_name.to_string();
                        let tx = self.timer_tx.clone();

                        log::debug!("{}: rescheduling to fire in {:?}", timer_name, delay);

                        tokio::spawn(async move {
                            timer_scheduler::watch_timer(timer_name, service_name, delay, tx).await;
                        });
                    }
                }
            }
        }
    }
}
