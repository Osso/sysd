//! Timer scheduler for .timer units
//!
//! Manages time-based service activation using tokio's sleep.

use crate::units::{CalendarSpec, Timer};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::sleep;

/// Message sent when a timer fires
#[derive(Debug)]
pub struct TimerFired {
    /// Name of the timer unit
    pub timer_name: String,
    /// Name of the service to start
    pub service_name: String,
}

/// Calculate next trigger time for a timer
pub fn calculate_next_trigger(timer: &Timer, boot_time: Instant) -> Option<Duration> {
    let now = Instant::now();
    let mut next: Option<Duration> = None;

    // Handle OnBootSec - time since boot
    if let Some(boot_sec) = timer.timer.on_boot_sec {
        let elapsed = now.duration_since(boot_time);
        if elapsed < boot_sec {
            let remaining = boot_sec - elapsed;
            next = Some(next.map_or(remaining, |n| n.min(remaining)));
        }
    }

    // Handle OnStartupSec - same as OnBootSec for us
    if let Some(startup_sec) = timer.timer.on_startup_sec {
        let elapsed = now.duration_since(boot_time);
        if elapsed < startup_sec {
            let remaining = startup_sec - elapsed;
            next = Some(next.map_or(remaining, |n| n.min(remaining)));
        }
    }

    // Handle OnActiveSec - time since timer was activated (approximated as boot time)
    if let Some(active_sec) = timer.timer.on_active_sec {
        next = Some(next.map_or(active_sec, |n| n.min(active_sec)));
    }

    // Handle OnUnitActiveSec - time after unit was last activated
    // For simplicity, trigger immediately on first run, then use the interval
    if let Some(unit_active_sec) = timer.timer.on_unit_active_sec {
        // On first activation, trigger after this duration
        next = Some(next.map_or(unit_active_sec, |n| n.min(unit_active_sec)));
    }

    // Handle OnCalendar - realtime calendar events
    for spec in &timer.timer.on_calendar {
        if let Some(cal_next) = next_calendar_trigger(spec) {
            next = Some(next.map_or(cal_next, |n| n.min(cal_next)));
        }
    }

    // Apply randomized delay if configured
    if let Some(random_delay) = timer.timer.randomized_delay_sec {
        if let Some(ref mut n) = next {
            // Add random delay between 0 and randomized_delay_sec
            let random_secs = rand_delay(random_delay.as_secs());
            *n += Duration::from_secs(random_secs);
        }
    }

    next
}

/// Calculate next trigger time for a calendar spec
fn next_calendar_trigger(spec: &CalendarSpec) -> Option<Duration> {
    use chrono::{Datelike, Local, Timelike, Weekday};

    let now = Local::now();

    match spec {
        CalendarSpec::Named(name) => match name.as_str() {
            "minutely" => {
                // Next minute
                let next = now + chrono::Duration::seconds(60 - i64::from(now.second()));
                Some(Duration::from_secs(
                    (next.timestamp() - now.timestamp()) as u64,
                ))
            }
            "hourly" => {
                // Next hour
                let secs_until_hour = 3600 - (now.minute() * 60 + now.second()) as u64;
                Some(Duration::from_secs(secs_until_hour))
            }
            "daily" => {
                // Tomorrow at 00:00
                let secs_until_midnight =
                    86400 - (now.hour() * 3600 + now.minute() * 60 + now.second()) as u64;
                Some(Duration::from_secs(secs_until_midnight))
            }
            "weekly" => {
                // Next Monday at 00:00
                let days_until_monday = (8 - now.weekday().num_days_from_monday()) % 7;
                let days_until_monday = if days_until_monday == 0 {
                    7
                } else {
                    days_until_monday
                };
                let secs_until_midnight =
                    86400 - (now.hour() * 3600 + now.minute() * 60 + now.second()) as u64;
                Some(Duration::from_secs(
                    secs_until_midnight + (days_until_monday as u64 - 1) * 86400,
                ))
            }
            "monthly" => {
                // First of next month at 00:00
                let next_month = if now.month() == 12 {
                    now.with_year(now.year() + 1)
                        .and_then(|d| d.with_month(1))
                        .and_then(|d| d.with_day(1))
                } else {
                    now.with_month(now.month() + 1)
                        .and_then(|d| d.with_day(1))
                };
                next_month.map(|next| {
                    let next = next
                        .with_hour(0)
                        .and_then(|d| d.with_minute(0))
                        .and_then(|d| d.with_second(0))
                        .unwrap_or(next);
                    Duration::from_secs((next.timestamp() - now.timestamp()) as u64)
                })
            }
            "yearly" | "annually" => {
                // January 1st of next year at 00:00
                let next_year = now
                    .with_year(now.year() + 1)
                    .and_then(|d| d.with_month(1))
                    .and_then(|d| d.with_day(1))
                    .and_then(|d| d.with_hour(0))
                    .and_then(|d| d.with_minute(0))
                    .and_then(|d| d.with_second(0));
                next_year
                    .map(|next| Duration::from_secs((next.timestamp() - now.timestamp()) as u64))
            }
            _ => None,
        },
        CalendarSpec::DayOfWeek(day) => {
            // Next occurrence of this day at 00:00
            let target_day = match day.to_lowercase().as_str() {
                "mon" | "monday" => Weekday::Mon,
                "tue" | "tuesday" => Weekday::Tue,
                "wed" | "wednesday" => Weekday::Wed,
                "thu" | "thursday" => Weekday::Thu,
                "fri" | "friday" => Weekday::Fri,
                "sat" | "saturday" => Weekday::Sat,
                "sun" | "sunday" => Weekday::Sun,
                _ => return None,
            };
            let current_day = now.weekday();
            let days_until = (target_day.num_days_from_monday() as i32
                - current_day.num_days_from_monday() as i32
                + 7)
                % 7;
            let days_until = if days_until == 0 { 7 } else { days_until };
            let secs_until_midnight =
                86400 - (now.hour() * 3600 + now.minute() * 60 + now.second()) as u64;
            Some(Duration::from_secs(
                secs_until_midnight + (days_until as u64 - 1) * 86400,
            ))
        }
        CalendarSpec::Time { hour, minute, second } => {
            // Today or tomorrow at the specified time
            let target_secs = (*hour * 3600 + *minute * 60 + *second) as u64;
            let now_secs = (now.hour() * 3600 + now.minute() * 60 + now.second()) as u64;
            if target_secs > now_secs {
                Some(Duration::from_secs(target_secs - now_secs))
            } else {
                // Tomorrow at the specified time
                Some(Duration::from_secs(86400 - now_secs + target_secs))
            }
        }
        CalendarSpec::Full(expr) => {
            // For complex expressions, parse common patterns
            if expr == "*-*-* *:00:00" {
                // Every hour at :00
                let secs_until_hour = 3600 - (now.minute() * 60 + now.second()) as u64;
                return Some(Duration::from_secs(secs_until_hour));
            }
            // For other expressions, fall back to 1 hour (simplified)
            log::warn!(
                "Complex calendar expression not fully supported: {}, using 1h fallback",
                expr
            );
            Some(Duration::from_secs(3600))
        }
    }
}

/// Generate a pseudo-random delay (simple hash-based)
fn rand_delay(max_secs: u64) -> u64 {
    if max_secs == 0 {
        return 0;
    }
    // Use current time as seed for simple randomization
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (now.as_nanos() as u64) % max_secs
}

/// Watch a timer and send activation message when it fires
pub async fn watch_timer(
    timer_name: String,
    service_name: String,
    delay: Duration,
    tx: mpsc::Sender<TimerFired>,
) {
    log::debug!(
        "{}: scheduling to fire in {:?}",
        timer_name,
        delay
    );

    sleep(delay).await;

    log::info!("{}: timer fired, activating {}", timer_name, service_name);

    if let Err(e) = tx
        .send(TimerFired {
            timer_name: timer_name.clone(),
            service_name: service_name.clone(),
        })
        .await
    {
        log::error!("{}: failed to send timer fired message: {}", timer_name, e);
    }
}
