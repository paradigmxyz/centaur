//! Console's scheduled-task cron schedules.
//!
//! Console accepts standard five-field cron (`minute hour day-of-month month
//! day-of-week`): numbers, three-letter month and weekday names, `*`, ranges,
//! steps, and lists. It validates them with the Ruby `fugit` gem, so the next
//! run computed here follows Fugit's reading of that grammar:
//!
//! - `0` and `7` are both Sunday.
//! - When both day fields are restricted, a day matching either one runs, as
//!   in crontab. Fugit treats the day-of-week field as restricted unless it is
//!   exactly `*`, and the day-of-month field as restricted unless an element
//!   covers every day.
//! - Months that have none of the listed days are dropped, so `30 2,4` runs
//!   only in April, also on the weekdays an `OR` would otherwise add.
//! - A local time that daylight saving skips never runs; a local time repeated
//!   when clocks fall back runs at both instants.

use std::{fmt, str::FromStr};

use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

/// The longest gap searched for a next occurrence: a February 29th schedule
/// can wait eight years for a leap year.
const MAX_SEARCH_DAYS: i64 = 366 * 9;

/// Days in each month, counting February's leap day.
const MAX_DAYS: [usize; 13] = [0, 31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// A schedule that is not standard five-field cron in a known timezone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CronError(String);

impl fmt::Display for CronError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CronError {}

/// A cron expression bound to the timezone its fields are read in.
#[derive(Clone, Debug)]
pub struct Schedule {
    minutes: [bool; 60],
    hours: [bool; 24],
    /// Indexed by day, 1 through 31.
    monthdays: [bool; 32],
    monthdays_restricted: bool,
    /// Indexed by month, 1 through 12.
    months: [bool; 13],
    /// Indexed from Sunday.
    weekdays: [bool; 7],
    weekdays_restricted: bool,
    zone: Tz,
}

/// One parsed list element: `*`, `n`, or `a-b`, each with an optional step.
struct Element {
    start: usize,
    end: usize,
    step: usize,
    /// Whether it covers the whole field one value at a time.
    full: bool,
}

impl Schedule {
    pub fn parse(expression: &str, timezone: &str) -> Result<Self, CronError> {
        let invalid = || CronError(format!("{expression:?} is not a five-field cron schedule"));
        let zone = Tz::from_str(timezone.trim())
            .map_err(|_| CronError(format!("{timezone:?} is not a supported schedule timezone")))?;
        let [minutes, hours, monthdays, months, weekdays] = expression
            .split_whitespace()
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| invalid())?;

        let mut schedule = Self {
            minutes: [false; 60],
            hours: [false; 24],
            monthdays: [false; 32],
            monthdays_restricted: false,
            months: [false; 13],
            weekdays: [false; 7],
            weekdays_restricted: weekdays != "*",
            zone,
        };
        fill(&mut schedule.minutes, minutes, 0, 59, None).ok_or_else(invalid)?;
        fill(&mut schedule.hours, hours, 0, 23, None).ok_or_else(invalid)?;
        schedule.monthdays_restricted =
            !fill(&mut schedule.monthdays, monthdays, 1, 31, None).ok_or_else(invalid)?;
        let months_restricted =
            !fill(&mut schedule.months, months, 1, 12, Some(&MONTHS)).ok_or_else(invalid)?;
        let mut weekdays_with_seven = [false; 8];
        fill(&mut weekdays_with_seven, weekdays, 0, 7, Some(&WEEKDAYS)).ok_or_else(invalid)?;
        for (day, selected) in weekdays_with_seven.iter().enumerate() {
            schedule.weekdays[day % 7] |= selected;
        }
        if months_restricted && schedule.monthdays_restricted && !schedule.compact_month_days() {
            return Err(invalid());
        }
        Ok(schedule)
    }

    /// Keeps only months with a listed day and days a listed month has; a
    /// schedule with no such pair never runs.
    fn compact_month_days(&mut self) -> bool {
        let mut months = [false; 13];
        let mut days = [false; 32];
        for month in (1..=12).filter(|&month| self.months[month]) {
            for day in (1..=MAX_DAYS[month]).filter(|&day| self.monthdays[day]) {
                months[month] = true;
                days[day] = true;
            }
        }
        self.months = months;
        self.monthdays = days;
        months.contains(&true)
    }

    /// The first occurrence strictly after `after`, at whole-minute precision.
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let after = after.with_nanosecond(0)?;
        let after_local = after.with_timezone(&self.zone).naive_local();
        for offset in 0..MAX_SEARCH_DAYS {
            let date = after_local
                .date()
                .checked_add_signed(Duration::days(offset))?;
            if !self.months[date.month() as usize] || !self.day_matches(date) {
                continue;
            }
            // The earliest instant, not the first local time: when clocks fall
            // back, a repeated hour's second pass follows later local times.
            let mut earliest: Option<DateTime<Utc>> = None;
            for hour in (0..24).filter(|&hour| self.hours[hour]) {
                // Hours this far before `after` cannot follow it, even across
                // a daylight saving change.
                if offset == 0 && hour + 2 < after_local.hour() as usize {
                    continue;
                }
                for minute in (0..60).filter(|&minute| self.minutes[minute]) {
                    let local = date.and_hms_opt(hour as u32, minute as u32, 0)?;
                    let instants = match self.zone.from_local_datetime(&local) {
                        LocalResult::Single(instant) => [Some(instant), None],
                        LocalResult::Ambiguous(earlier, later) => [Some(earlier), Some(later)],
                        LocalResult::None => continue,
                    };
                    for instant in instants.into_iter().flatten() {
                        let instant = instant.with_timezone(&Utc);
                        if instant > after && earliest.is_none_or(|earliest| instant < earliest) {
                            earliest = Some(instant);
                        }
                    }
                }
            }
            if earliest.is_some() {
                return earliest;
            }
        }
        None
    }

    fn day_matches(&self, date: NaiveDate) -> bool {
        let weekday = self.weekdays[date.weekday().num_days_from_sunday() as usize];
        let monthday = self.monthdays[date.day() as usize];
        if self.weekdays_restricted && self.monthdays_restricted {
            weekday || monthday
        } else {
            weekday && monthday
        }
    }
}

/// Marks the values a field selects. Returns whether an element covers every
/// value, or `None` when the field is invalid.
fn fill(
    selected: &mut [bool],
    field: &str,
    min: usize,
    max: usize,
    names: Option<&[&str]>,
) -> Option<bool> {
    let elements: Vec<&str> = field.split(',').collect();
    let mut full = false;
    for text in &elements {
        let element = parse_element(text, min, max, names)?;
        // `*` combines with nothing else, so lists never mean "every value".
        if text.starts_with('*') && elements.len() > 1 {
            return None;
        }
        full |= element.full;
        for value in (element.start..=element.end).step_by(element.step) {
            selected[value] = true;
        }
    }
    Some(full)
}

fn parse_element(text: &str, min: usize, max: usize, names: Option<&[&str]>) -> Option<Element> {
    let (range, step) = match text.split_once('/') {
        Some((range, step)) => (range, Some(step)),
        None => (text, None),
    };
    let (start, end) = if range == "*" {
        (min, max)
    } else {
        match range.split_once('-') {
            Some((start, end)) => (value(start, min, max, names)?, value(end, min, max, names)?),
            // A step needs a range: Fugit reads `5/15` differently by field.
            None if step.is_some() => return None,
            None => {
                let value = value(range, min, max, names)?;
                (value, value)
            }
        }
    };
    if start > end {
        return None;
    }
    let step = match step {
        Some(step) => number(step)?,
        None => 1,
    };
    if step == 0 {
        return None;
    }
    let full = start == min && end == max && step == 1;
    Some(Element {
        start,
        end,
        step,
        full,
    })
}

fn value(text: &str, min: usize, max: usize, names: Option<&[&str]>) -> Option<usize> {
    let value = match number(text) {
        Some(value) => value,
        None => {
            let names = names?;
            let index = names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(text))?;
            // Weekday names count from Sunday = 0, month names from January = 1.
            if names.len() == MONTHS.len() {
                index + 1
            } else {
                index
            }
        }
    };
    (min..=max).contains(&value).then_some(value)
}

/// One or two ASCII digits.
fn number(text: &str) -> Option<usize> {
    if text.is_empty() || text.len() > 2 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn next(expression: &str, timezone: &str, after: &str) -> String {
        Schedule::parse(expression, timezone)
            .unwrap_or_else(|error| panic!("{expression}: {error}"))
            .next_after(at(after))
            .map(|time| time.format("%Y-%m-%dT%H:%M:%SZ").to_string())
            .unwrap_or_default()
    }

    #[test]
    fn rejects_anything_but_standard_five_field_cron() {
        for expression in [
            "",
            "0 9 * *",
            "0 0 9 * * *",
            "@daily",
            "0 9 L * *",
            "0 9 * * 1#2",
            "0 9 * * mon%2",
            "0 9 * * 1&",
            "~ 9 * * *",
            "60 9 * * *",
            "0 24 * * *",
            "0 9 0 * *",
            "0 9 32 * *",
            "0 9 -1 * *",
            "0 9 * 0 *",
            "0 9 * 13 *",
            "0 9 * * 8",
            "0 9 * * 5-1",
            "5/15 9 * * *",
            "*/0 9 * * *",
            "0 9 *,1 * *",
            "0 9 ,1 * *",
            "0 9 1, * *",
            "0 9 * * monday",
            "0 9 30 2 *",
            "0 9 31 4,6 *",
            "0 9 * * 001",
        ] {
            assert!(
                Schedule::parse(expression, "UTC").is_err(),
                "{expression:?} should be rejected"
            );
        }
        assert!(Schedule::parse("0 9 * * *", "PST").is_err());
        assert!(Schedule::parse("0 9 * * *", "Europe/Bogus").is_err());
    }

    /// Expected times are `Fugit::Cron.parse("#{expression} #{zone}").next_time(after)`
    /// from fugit 1.14.
    #[test]
    fn next_times_match_fugit() {
        let cases = [
            // Every schedule in production as of this migration.
            ("0 18 * * 0", "2026-08-19T12:00:00Z", "2026-08-24T01:00:00Z"),
            ("0 13 * * 1", "2026-08-19T12:00:00Z", "2026-08-24T20:00:00Z"),
            ("0 6 * * *", "2026-08-19T12:00:00Z", "2026-08-19T13:00:00Z"),
            ("0 9 * * 5", "2026-08-19T12:00:00Z", "2026-08-21T16:00:00Z"),
            ("0 9 * * 1", "2026-08-19T12:00:00Z", "2026-08-24T16:00:00Z"),
            ("0 8 * * 1", "2026-08-19T12:00:00Z", "2026-08-24T15:00:00Z"),
            (
                "30 16 * * 1-5",
                "2026-08-19T12:00:00Z",
                "2026-08-19T23:30:00Z",
            ),
            ("35 9 * * *", "2026-08-19T12:00:00Z", "2026-08-19T16:35:00Z"),
            (
                "40 22 * * *",
                "2026-08-19T12:00:00Z",
                "2026-08-20T05:40:00Z",
            ),
            (
                "0 9 * * 1-5",
                "2026-08-21T17:00:00Z",
                "2026-08-24T16:00:00Z",
            ),
            ("0 4 * * *", "2026-08-19T12:00:00Z", "2026-08-20T11:00:00Z"),
            ("0 10 * * *", "2026-08-19T12:00:00Z", "2026-08-19T17:00:00Z"),
            (
                "*/15 * * * *",
                "2026-08-19T12:07:30Z",
                "2026-08-19T12:15:00Z",
            ),
            ("0 7 * * *", "2026-08-19T12:00:00Z", "2026-08-19T14:00:00Z"),
            ("0 8 * * *", "2026-08-19T12:00:00Z", "2026-08-19T15:00:00Z"),
            ("0 * * * *", "2026-08-19T12:00:00Z", "2026-08-19T13:00:00Z"),
            ("0 16 * * 5", "2026-08-19T12:00:00Z", "2026-08-21T23:00:00Z"),
            // Grammar this module accepts beyond those.
            ("0 9 * * 7", "2026-08-19T12:00:00Z", "2026-08-23T16:00:00Z"),
            (
                "0 9 * * MON-FRI",
                "2026-08-22T12:00:00Z",
                "2026-08-24T16:00:00Z",
            ),
            (
                "0 9 1 jan *",
                "2026-08-19T12:00:00Z",
                "2027-01-01T17:00:00Z",
            ),
            (
                "0 9 * * 5-7",
                "2026-08-21T17:00:00Z",
                "2026-08-22T16:00:00Z",
            ),
            (
                "0 9 * * */2",
                "2026-08-19T17:00:00Z",
                "2026-08-20T16:00:00Z",
            ),
            (
                "0 9 * * 1-5/2",
                "2026-08-19T17:00:00Z",
                "2026-08-21T16:00:00Z",
            ),
            (
                "0 9-17/4 * * *",
                "2026-08-19T17:00:00Z",
                "2026-08-19T20:00:00Z",
            ),
            (
                "5,35 * * * *",
                "2026-08-19T12:10:00Z",
                "2026-08-19T12:35:00Z",
            ),
            // Either day field matches when both are restricted...
            ("0 9 1 * 1", "2026-08-19T12:00:00Z", "2026-08-24T16:00:00Z"),
            ("0 9 25 * 1", "2026-08-24T17:00:00Z", "2026-08-25T16:00:00Z"),
            // ...including a day-of-week field that lists every day.
            (
                "0 9 1 * 0-6",
                "2026-08-19T17:00:00Z",
                "2026-08-20T16:00:00Z",
            ),
            // A day-of-month field that covers every day is unrestricted.
            (
                "0 9 1-31 * 1",
                "2026-08-19T17:00:00Z",
                "2026-08-24T16:00:00Z",
            ),
            // Months without the listed day are dropped before the OR.
            (
                "0 9 30 2,4 1",
                "2026-01-01T00:00:00Z",
                "2026-04-06T16:00:00Z",
            ),
            ("0 9 29 2 *", "2026-03-01T00:00:00Z", "2028-02-29T17:00:00Z"),
            // A whole number of seconds after a run is not that run.
            (
                "0 * * * *",
                "2026-08-19T12:00:00.500Z",
                "2026-08-19T13:00:00Z",
            ),
            // Spring forward skips 02:30; fall back runs 01:30 twice.
            ("30 2 * * *", "2026-03-08T00:00:00Z", "2026-03-09T09:30:00Z"),
            ("30 1 * * *", "2026-11-01T07:00:00Z", "2026-11-01T08:30:00Z"),
            ("30 1 * * *", "2026-11-01T08:30:00Z", "2026-11-01T09:30:00Z"),
            ("30 1 * * *", "2026-11-01T09:30:00Z", "2026-11-02T09:30:00Z"),
            ("30 * * * *", "2026-11-01T08:30:00Z", "2026-11-01T09:30:00Z"),
            (
                "*/15 * * * *",
                "2026-11-01T08:15:00Z",
                "2026-11-01T08:30:00Z",
            ),
            ("0 * * * *", "2026-11-01T08:15:00Z", "2026-11-01T09:00:00Z"),
        ];
        for (expression, after, expected) in cases {
            assert_eq!(
                next(expression, "America/Los_Angeles", after),
                expected,
                "{expression} after {after}"
            );
        }
        assert_eq!(
            next("0 9 * * 1", "UTC", "2026-08-19T12:00:00Z"),
            "2026-08-24T09:00:00Z"
        );
    }
}
