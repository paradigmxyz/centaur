//! When to refresh next. Console's `BrokerCredential#compute_next_attempt_at`
//! schedules a credential's first refresh after the consent flow with the same
//! formula, without the one-minute minimum interval.

use chrono::{NaiveDateTime, TimeDelta};

/// The access token keeps at least this much life past the scheduled refresh.
const REFRESH_FLOOR_SECONDS: f64 = 60.0;
/// Used when the token endpoint omits `expires_in`.
const DEFAULT_EXPIRES_IN_SECONDS: i64 = 5 * 60;
/// Caps `expires_in` so timestamp arithmetic cannot overflow.
const MAX_EXPIRES_IN_SECONDS: i64 = 10 * 365 * 24 * 60 * 60;
/// Tokens that live shorter than the refresh floor are refreshed at most this
/// often, the cadence of the Console job this service replaces.
const MIN_REFRESH_INTERVAL_SECONDS: i64 = 60;
const BACKOFF_BASE_SECONDS: i64 = 5;
const BACKOFF_MAX_SECONDS: i64 = 5 * 60;

/// Per-credential refresh settings from `broker_credentials`.
pub(crate) struct Schedule {
    pub early_refresh_slack_seconds: i32,
    pub early_refresh_fraction: f64,
    pub max_refresh_interval_seconds: i32,
}

/// The token's lifetime in seconds, defaulted and bounded.
pub(crate) fn expires_in(reported: Option<i64>) -> i64 {
    match reported {
        Some(seconds) if seconds > 0 => seconds.min(MAX_EXPIRES_IN_SECONDS),
        _ => DEFAULT_EXPIRES_IN_SECONDS,
    }
}

/// After a refresh at `now` minting a token that lives `expires_in` seconds:
/// the earlier of the early trigger (the larger of the slack and the lifetime
/// fraction, at least the floor, before expiry) and the maximum interval.
pub(crate) fn next_attempt_at(
    now: NaiveDateTime,
    expires_in: i64,
    schedule: &Schedule,
) -> NaiveDateTime {
    let mut slack = f64::from(schedule.early_refresh_slack_seconds);
    if schedule.early_refresh_fraction > 0.0 {
        slack = slack.max(expires_in as f64 * schedule.early_refresh_fraction);
    }
    slack = slack.max(REFRESH_FLOOR_SECONDS);
    let early =
        now + TimeDelta::microseconds(((expires_in as f64 - slack) * 1_000_000.0).round() as i64);
    let ceiling = now + TimeDelta::seconds(i64::from(schedule.max_refresh_interval_seconds));
    early
        .min(ceiling)
        .max(now + TimeDelta::seconds(MIN_REFRESH_INTERVAL_SECONDS))
}

/// Exponential backoff after the `failures`th consecutive retryable failure.
pub(crate) fn retry_at(now: NaiveDateTime, failures: i32) -> NaiveDateTime {
    let exponent = (failures - 1).clamp(0, 6) as u32;
    now + TimeDelta::seconds((BACKOFF_BASE_SECONDS * 2_i64.pow(exponent)).min(BACKOFF_MAX_SECONDS))
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, NaiveDateTime, TimeDelta};

    use super::{Schedule, expires_in, next_attempt_at, retry_at};

    fn now() -> NaiveDateTime {
        DateTime::from_timestamp(1_800_000_000, 0)
            .unwrap()
            .naive_utc()
    }

    fn schedule(slack: i32, fraction: f64, max_interval: i32) -> Schedule {
        Schedule {
            early_refresh_slack_seconds: slack,
            early_refresh_fraction: fraction,
            max_refresh_interval_seconds: max_interval,
        }
    }

    #[test]
    fn refreshes_before_expiry_by_the_larger_slack() {
        // A 3600s token: the 0.2 fraction (720s) beats the 300s slack.
        assert_eq!(
            next_attempt_at(now(), 3600, &schedule(300, 0.2, 86_400)),
            now() + TimeDelta::seconds(3600 - 720)
        );
        assert_eq!(
            next_attempt_at(now(), 3600, &schedule(300, 0.0, 86_400)),
            now() + TimeDelta::seconds(3300)
        );
        // Never closer than the 60s floor to expiry.
        assert_eq!(
            next_attempt_at(now(), 3600, &schedule(10, 0.0, 86_400)),
            now() + TimeDelta::seconds(3540)
        );
    }

    #[test]
    fn caps_long_lived_tokens_at_the_maximum_interval() {
        assert_eq!(
            next_attempt_at(now(), 100_000, &schedule(10, 0.0, 100)),
            now() + TimeDelta::seconds(100)
        );
    }

    #[test]
    fn short_lived_tokens_refresh_at_most_once_a_minute() {
        assert_eq!(
            next_attempt_at(now(), expires_in(None), &schedule(300, 0.2, 86_400)),
            now() + TimeDelta::seconds(60)
        );
    }

    #[test]
    fn defaults_and_bounds_the_lifetime() {
        assert_eq!(expires_in(None), 300);
        assert_eq!(expires_in(Some(0)), 300);
        assert_eq!(expires_in(Some(3600)), 3600);
        assert_eq!(expires_in(Some(i64::MAX)), 10 * 365 * 24 * 60 * 60);
    }

    #[test]
    fn backs_off_exponentially_up_to_five_minutes() {
        let delays: Vec<i64> = [1, 2, 3, 6, 7, 50]
            .into_iter()
            .map(|failures| (retry_at(now(), failures) - now()).num_seconds())
            .collect();
        assert_eq!(delays, [5, 10, 20, 160, 300, 300]);
    }
}
