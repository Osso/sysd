use super::*;
use chrono::TimeZone;

fn local_time(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> chrono::DateTime<chrono::Local> {
    chrono::Local
        .with_ymd_and_hms(year, month, day, hour, minute, second)
        .single()
        .unwrap()
}

fn timer_with_name(name: &str) -> Timer {
    Timer::new(name.to_string())
}

#[test]
fn calculate_next_trigger_uses_shortest_monotonic_delay() {
    let mut timer = timer_with_name("backup.timer");
    timer.timer.on_boot_sec = Some(Duration::from_secs(60));
    timer.timer.on_startup_sec = Some(Duration::from_secs(45));
    timer.timer.on_active_sec = Some(Duration::from_secs(30));
    timer.timer.on_unit_active_sec = Some(Duration::from_secs(90));

    let delay = calculate_next_trigger(&timer, Instant::now()).unwrap();

    assert!(delay <= Duration::from_secs(30));
    assert!(delay > Duration::ZERO);
}

#[test]
fn calculate_next_trigger_ignores_elapsed_boot_and_startup_timers() {
    let mut timer = timer_with_name("elapsed.timer");
    timer.timer.on_boot_sec = Some(Duration::from_secs(1));
    timer.timer.on_startup_sec = Some(Duration::from_secs(2));

    let delay = calculate_next_trigger(&timer, Instant::now() - Duration::from_secs(5));

    assert_eq!(delay, None);
}

#[test]
fn calculate_next_trigger_applies_randomized_delay_within_bound() {
    let mut timer = timer_with_name("random.timer");
    timer.timer.on_active_sec = Some(Duration::from_secs(10));
    timer.timer.randomized_delay_sec = Some(Duration::from_secs(5));

    let delay = calculate_next_trigger(&timer, Instant::now()).unwrap();

    assert!(delay >= Duration::from_secs(10));
    assert!(delay < Duration::from_secs(15));
}

#[test]
fn named_calendar_triggers_cover_common_schedules() {
    let now = local_time(2026, 1, 12, 10, 30, 15);

    assert_eq!(
        next_named_trigger(&now, "minutely"),
        Some(Duration::from_secs(45))
    );
    assert_eq!(
        next_named_trigger(&now, "hourly"),
        Some(Duration::from_secs(1785))
    );
    assert_eq!(
        next_named_trigger(&now, "daily"),
        Some(Duration::from_secs(48585))
    );
    assert_eq!(
        next_named_trigger(&now, "weekly"),
        Some(Duration::from_secs(566985))
    );
    assert_eq!(next_named_trigger(&now, "quarterly"), None);
}

#[test]
fn weekday_and_time_triggers_wrap_to_next_occurrence() {
    let now = local_time(2026, 1, 12, 10, 30, 15);

    assert_eq!(
        next_day_of_week_trigger(&now, "Tue"),
        Some(Duration::from_secs(48585))
    );
    assert_eq!(
        next_day_of_week_trigger(&now, "monday"),
        Some(Duration::from_secs(566985))
    );
    assert_eq!(next_day_of_week_trigger(&now, "Funday"), None);
    assert_eq!(
        next_time_trigger(&now, 10, 31, 0),
        Duration::from_secs(45)
    );
    assert_eq!(
        next_time_trigger(&now, 9, 0, 0),
        Duration::from_secs(80985)
    );
}

#[test]
fn full_calendar_expressions_use_supported_hourly_or_fallback() {
    let now = local_time(2026, 1, 12, 10, 30, 15);

    assert_eq!(
        next_full_expression_trigger(&now, "*-*-* *:00:00"),
        Some(Duration::from_secs(1785))
    );
    assert_eq!(
        next_full_expression_trigger(&now, "Mon..Fri 08:00"),
        Some(Duration::from_secs(3600))
    );
}

#[test]
fn monthly_and_yearly_triggers_align_to_next_boundary() {
    let now = local_time(2026, 1, 12, 10, 30, 15);
    let december = local_time(2026, 12, 31, 23, 59, 0);

    assert_eq!(
        next_monthly_trigger(&now),
        Some(Duration::from_secs(1_690_185))
    );
    assert_eq!(
        next_monthly_trigger(&december),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        next_yearly_trigger(&december),
        Some(Duration::from_secs(60))
    );
}

#[test]
fn weekday_parser_accepts_short_and_long_names() {
    assert_eq!(parse_weekday("mon"), Some(chrono::Weekday::Mon));
    assert_eq!(parse_weekday("tuesday"), Some(chrono::Weekday::Tue));
    assert_eq!(parse_weekday("wed"), Some(chrono::Weekday::Wed));
    assert_eq!(parse_weekday("thursday"), Some(chrono::Weekday::Thu));
    assert_eq!(parse_weekday("fri"), Some(chrono::Weekday::Fri));
    assert_eq!(parse_weekday("saturday"), Some(chrono::Weekday::Sat));
    assert_eq!(parse_weekday("sun"), Some(chrono::Weekday::Sun));
    assert_eq!(parse_weekday("holiday"), None);
}

#[test]
fn seconds_helpers_report_midnight_offsets() {
    let now = local_time(2026, 1, 12, 10, 30, 15);
    let tomorrow = local_time(2026, 1, 13, 10, 30, 15);

    assert_eq!(seconds_since_midnight(&now), 37_815);
    assert_eq!(seconds_until_midnight(&now), 48_585);
    assert_eq!(
        align_to_midnight(tomorrow).unwrap(),
        local_time(2026, 1, 13, 0, 0, 0)
    );
}

#[test]
fn rand_delay_is_zero_or_below_upper_bound() {
    assert_eq!(rand_delay(0), 0);
    assert!(rand_delay(10) < 10);
}

#[tokio::test]
async fn watch_timer_sends_timer_activation_message() {
    let (tx, mut rx) = mpsc::channel(1);

    watch_timer(
        "backup.timer".to_string(),
        "backup.service".to_string(),
        Duration::ZERO,
        tx,
    )
    .await;

    let fired = rx.recv().await.unwrap();
    assert_eq!(fired.timer_name, "backup.timer");
    assert_eq!(fired.service_name, "backup.service");
}

#[tokio::test]
async fn watch_timer_handles_closed_receiver() {
    let (tx, rx) = mpsc::channel(1);
    drop(rx);

    watch_timer(
        "closed.timer".to_string(),
        "closed.service".to_string(),
        Duration::ZERO,
        tx,
    )
    .await;
}
