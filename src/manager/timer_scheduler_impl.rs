// Timer scheduler for .timer units
//
// Manages time-based service activation using tokio's sleep.

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
    let now = chrono::Local::now();
    match spec {
        CalendarSpec::Named(name) => next_named_trigger(&now, name),
        CalendarSpec::DayOfWeek(day) => next_day_of_week_trigger(&now, day),
        CalendarSpec::Time {
            hour,
            minute,
            second,
        } => Some(next_time_trigger(&now, *hour, *minute, *second)),
        CalendarSpec::Full(expr) => next_full_expression_trigger(&now, expr),
    }
}

fn next_named_trigger(now: &chrono::DateTime<chrono::Local>, name: &str) -> Option<Duration> {
    match name {
        "minutely" => Some(Duration::from_secs(60 - seconds_since_midnight(now) % 60)),
        "hourly" => Some(Duration::from_secs(3600 - seconds_since_midnight(now) % 3600)),
        "daily" => Some(Duration::from_secs(seconds_until_midnight(now))),
        "weekly" => Some(Duration::from_secs(seconds_until_weekday(now, chrono::Weekday::Mon))),
        "monthly" => next_monthly_trigger(now),
        "yearly" | "annually" => next_yearly_trigger(now),
        _ => None,
    }
}

fn next_day_of_week_trigger(
    now: &chrono::DateTime<chrono::Local>,
    day: &str,
) -> Option<Duration> {
    let target = parse_weekday(day)?;
    Some(Duration::from_secs(seconds_until_weekday(now, target)))
}

fn next_time_trigger(
    now: &chrono::DateTime<chrono::Local>,
    hour: u32,
    minute: u32,
    second: u32,
) -> Duration {
    let target_secs = (hour * 3600 + minute * 60 + second) as u64;
    let now_secs = seconds_since_midnight(now);
    if target_secs > now_secs {
        Duration::from_secs(target_secs - now_secs)
    } else {
        Duration::from_secs(86400 - now_secs + target_secs)
    }
}

fn next_full_expression_trigger(
    now: &chrono::DateTime<chrono::Local>,
    expr: &str,
) -> Option<Duration> {
    if expr == "*-*-* *:00:00" {
        return Some(Duration::from_secs(3600 - seconds_since_midnight(now) % 3600));
    }

    log::warn!(
        "Complex calendar expression not fully supported: {}, using 1h fallback",
        expr
    );
    Some(Duration::from_secs(3600))
}

fn next_monthly_trigger(now: &chrono::DateTime<chrono::Local>) -> Option<Duration> {
    use chrono::Datelike;

    let next_month = if now.month() == 12 {
        now.with_year(now.year() + 1)
            .and_then(|d| d.with_month(1))
            .and_then(|d| d.with_day(1))
    } else {
        now.with_month(now.month() + 1).and_then(|d| d.with_day(1))
    }?;
    let aligned = align_to_midnight(next_month).unwrap_or(next_month);
    Some(Duration::from_secs((aligned.timestamp() - now.timestamp()) as u64))
}

fn next_yearly_trigger(now: &chrono::DateTime<chrono::Local>) -> Option<Duration> {
    use chrono::Datelike;

    let next_year = now
        .with_year(now.year() + 1)
        .and_then(|d| d.with_month(1))
        .and_then(|d| d.with_day(1))
        .and_then(align_to_midnight)?;
    Some(Duration::from_secs((next_year.timestamp() - now.timestamp()) as u64))
}

fn parse_weekday(day: &str) -> Option<chrono::Weekday> {
    use chrono::Weekday;

    match day.to_lowercase().as_str() {
        "mon" | "monday" => Some(Weekday::Mon),
        "tue" | "tuesday" => Some(Weekday::Tue),
        "wed" | "wednesday" => Some(Weekday::Wed),
        "thu" | "thursday" => Some(Weekday::Thu),
        "fri" | "friday" => Some(Weekday::Fri),
        "sat" | "saturday" => Some(Weekday::Sat),
        "sun" | "sunday" => Some(Weekday::Sun),
        _ => None,
    }
}

fn seconds_until_weekday(
    now: &chrono::DateTime<chrono::Local>,
    target: chrono::Weekday,
) -> u64 {
    use chrono::Datelike;

    let current = now.weekday().num_days_from_monday() as i32;
    let target = target.num_days_from_monday() as i32;
    let days_until = (target - current + 7) % 7;
    let days_until = if days_until == 0 { 7 } else { days_until as u64 };
    seconds_until_midnight(now) + (days_until - 1) * 86400
}

fn seconds_since_midnight(now: &chrono::DateTime<chrono::Local>) -> u64 {
    use chrono::Timelike;
    (now.hour() * 3600 + now.minute() * 60 + now.second()) as u64
}

fn seconds_until_midnight(now: &chrono::DateTime<chrono::Local>) -> u64 {
    86400 - seconds_since_midnight(now)
}

fn align_to_midnight(
    date: chrono::DateTime<chrono::Local>,
) -> Option<chrono::DateTime<chrono::Local>> {
    use chrono::Timelike;
    date.with_hour(0)
        .and_then(|d| d.with_minute(0))
        .and_then(|d| d.with_second(0))
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
    log::debug!("{}: scheduling to fire in {:?}", timer_name, delay);

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
