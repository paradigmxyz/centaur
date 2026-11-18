//! Idempotency keys and the wire time format (RFC 0006a "Time format" and
//! "Idempotency keys").

use chrono::{DateTime, SubsecRound, Utc};

/// `scheduled-task-tick:{id}:{scheduled_for}`: one tick per occurrence.
pub fn tick_key(task_id: i64, scheduled_for: DateTime<Utc>) -> String {
    format!("scheduled-task-tick:{task_id}:{}", key_time(scheduled_for))
}

/// `scheduled-task-tick:{id}:manual:{scheduled_for}`: a "run now" tick.
pub fn manual_tick_key(task_id: i64, scheduled_for: DateTime<Utc>) -> String {
    format!(
        "scheduled-task-tick:{task_id}:manual:{}",
        key_time(scheduled_for)
    )
}

/// `ScheduledTaskRunJob#idempotency_key`:
/// `"scheduled-task:#{task.id}:#{Time.iso8601(scheduled_for.to_s).utc.iso8601}"`.
pub fn run_key(task_id: i64, scheduled_for: DateTime<Utc>) -> String {
    format!("scheduled-task:{task_id}:{}", key_time(scheduled_for))
}

/// Ruby's `Time#utc.iso8601`: whole seconds, fractions truncated, `Z`.
pub fn key_time(time: DateTime<Utc>) -> String {
    time.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// RFC 3339 UTC with `Z`, fractional seconds (microseconds) only when
/// non-zero.
pub fn wire_time(time: DateTime<Utc>) -> String {
    let time = to_micros(time);
    if time.timestamp_subsec_micros() == 0 {
        key_time(time)
    } else {
        time.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
    }
}

/// Parses any RFC 3339 time to UTC at the database's microsecond precision.
pub fn parse_wire_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|time| to_micros(time.with_timezone(&Utc)))
}

/// Truncates to the microsecond precision of `timestamp(6)` columns.
pub fn to_micros(time: DateTime<Utc>) -> DateTime<Utc> {
    time.trunc_subsecs(6)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    #[test]
    fn run_keys_match_rails_scheduled_task_run_job() {
        // ScheduledTaskRunJob test: "scheduled-task:#{task.id}:2026-08-19T12:00:00Z"
        let at = Utc.with_ymd_and_hms(2026, 8, 19, 12, 0, 0).unwrap();
        assert_eq!(run_key(42, at), "scheduled-task:42:2026-08-19T12:00:00Z");
        // Time.iso8601(...).utc.iso8601 truncates fractions.
        let fractional = parse_wire_time("2026-08-19T05:00:00.987654-07:00").unwrap();
        assert_eq!(
            run_key(42, fractional),
            "scheduled-task:42:2026-08-19T12:00:00Z"
        );
    }

    #[test]
    fn tick_keys_separate_manual_runs() {
        let at = Utc.with_ymd_and_hms(2026, 8, 19, 12, 0, 0).unwrap();
        assert_eq!(
            tick_key(7, at),
            "scheduled-task-tick:7:2026-08-19T12:00:00Z"
        );
        assert_eq!(
            manual_tick_key(7, at),
            "scheduled-task-tick:7:manual:2026-08-19T12:00:00Z"
        );
    }

    #[test]
    fn wire_times_keep_microseconds_only_when_present() {
        let at = Utc.with_ymd_and_hms(2026, 8, 19, 12, 0, 0).unwrap();
        assert_eq!(wire_time(at), "2026-08-19T12:00:00Z");
        let fractional = parse_wire_time("2026-08-19T12:00:00.25Z").unwrap();
        assert_eq!(wire_time(fractional), "2026-08-19T12:00:00.250000Z");
        let nanos = parse_wire_time("2026-08-19T12:00:00.123456789Z").unwrap();
        assert_eq!(wire_time(nanos), "2026-08-19T12:00:00.123456Z");
        assert_eq!(parse_wire_time("not a time"), None);
    }
}
