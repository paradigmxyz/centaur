//! Cron schedules for scheduled tasks, on `croner`: validation, the next
//! occurrence, the console's schedule presets, and the labels the UI shows.
//!
//! Used by the console API (create/update/run) and the api-rs runner's
//! tests. A schedule is a standard five-field cron expression plus an IANA
//! time zone name.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;

pub const DEFAULT_TIMEZONE: &str = "America/Los_Angeles";

/// `(preset, cron expression)`.
pub const PRESETS: [(&str, &str); 4] = [
    ("daily", "0 9 * * *"),
    ("weekdays", "0 9 * * 1-5"),
    ("mondays", "0 9 * * 1"),
    ("fridays", "0 9 * * 5"),
];

/// Weekday `(label, cron day-of-week)` in display order (Monday first).
pub const DAYS: [(&str, &str); 7] = [
    ("Mon", "1"),
    ("Tue", "2"),
    ("Wed", "3"),
    ("Thu", "4"),
    ("Fri", "5"),
    ("Sat", "6"),
    ("Sun", "0"),
];

pub const INVALID_CRON_MESSAGE: &str = "is not a valid cron schedule";

/// Trimmed expression; blank means "manual only" (`None`).
pub fn normalize_cron_expression(value: Option<&str>) -> Option<String> {
    let value = value.unwrap_or_default().trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn parse(cron: &str) -> Option<Cron> {
    cron.trim().parse().ok()
}

fn zone(timezone: &str) -> Option<Tz> {
    timezone.trim().parse().ok()
}

/// Whether `cron` in `timezone` is a usable schedule.
pub fn is_valid(cron: &str, timezone: &str) -> bool {
    parse(cron).is_some() && zone(timezone).is_some()
}

/// The validation message for a task's schedule, if it has one and it is
/// not valid.
pub fn schedule_error(cron: Option<&str>, timezone: &str) -> Option<&'static str> {
    let cron = cron.filter(|cron| !cron.trim().is_empty())?;
    (!is_valid(cron, timezone)).then_some(INVALID_CRON_MESSAGE)
}

/// The first occurrence strictly after `after`, or `None` when the schedule
/// is invalid or never fires again.
pub fn next_run(cron: &str, timezone: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let (cron, zone) = (parse(cron)?, zone(timezone)?);
    cron.find_next_occurrence(&after.with_timezone(&zone), false)
        .ok()
        .map(|next| next.with_timezone(&Utc))
}

/// The next run of an enabled task that has a schedule.
pub fn next_run_if_enabled(
    enabled: bool,
    cron: Option<&str>,
    timezone: &str,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let cron = cron.filter(|cron| !cron.trim().is_empty())?;
    enabled.then(|| next_run(cron, timezone, now)).flatten()
}

/// The cron expression for a console schedule choice: `manual` (none), a
/// preset name, `cron` (the given expression), or `custom` (weekdays plus
/// `HH:MM`). An incomplete custom choice yields an expression that fails
/// validation, so the form reports it.
pub fn cron_for_choice(
    preset: &str,
    expression: Option<&str>,
    days: &[String],
    time: Option<&str>,
) -> Option<String> {
    match preset {
        "manual" => None,
        "cron" => Some(expression.unwrap_or_default().trim().to_owned()),
        "custom" => {
            Some(custom_cron(days, time).unwrap_or_else(|| "invalid custom schedule".to_owned()))
        }
        preset => Some(
            PRESETS
                .iter()
                .find(|(name, _)| *name == preset)
                .map_or_else(
                    || expression.unwrap_or_default().trim().to_owned(),
                    |(_, cron)| (*cron).to_owned(),
                ),
        ),
    }
}

/// `M H * * d,d` for selected cron weekdays and an `HH:MM` time.
pub fn custom_cron(days: &[String], time: Option<&str>) -> Option<String> {
    let (hour, minute) = parse_time(time?)?;
    let days: Vec<&str> = DAYS
        .iter()
        .map(|(_, day)| *day)
        .filter(|day| days.iter().any(|selected| selected == day))
        .collect();
    (!days.is_empty()).then(|| format!("{minute} {hour} * * {}", days.join(",")))
}

fn parse_time(time: &str) -> Option<(u32, u32)> {
    let (hour, minute) = time.split_once(':')?;
    if hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    let (hour, minute) = (hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?);
    (hour < 24 && minute < 60).then_some((hour, minute))
}

/// A schedule the console can edit as weekdays and a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Weekly {
    /// Cron weekdays, Monday first.
    pub days: Vec<&'static str>,
    /// `HH:MM`.
    pub time: String,
}

/// Reads `M H * * days` (digits, `a-b` ranges, commas, or `*`).
pub fn weekly(cron: Option<&str>) -> Option<Weekly> {
    let fields: Vec<&str> = cron?.split_whitespace().collect();
    let [minute, hour, "*", "*", days] = fields[..] else {
        return None;
    };
    let (minute, hour) = (minute.parse::<u32>().ok()?, hour.parse::<u32>().ok()?);
    if minute > 59 || hour > 23 || !minute_hour_plain(&fields) {
        return None;
    }
    let mut selected = Vec::new();
    if days == "*" {
        selected.extend(0..7u8);
    } else {
        for part in days.split(',') {
            let (from, to) = match part.split_once('-') {
                Some((from, to)) => (day(from)?, day(to)?),
                None => (day(part)?, day(part)?),
            };
            selected.extend(from..=to);
        }
    }
    let days: Vec<&'static str> = DAYS
        .iter()
        .map(|(_, cron_day)| *cron_day)
        .filter(|cron_day| {
            selected
                .iter()
                .any(|d| *d % 7 == cron_day.parse::<u8>().unwrap_or(u8::MAX))
        })
        .collect();
    (!days.is_empty()).then(|| Weekly {
        days,
        time: format!("{hour:02}:{minute:02}"),
    })
}

fn minute_hour_plain(fields: &[&str]) -> bool {
    fields[..2]
        .iter()
        .all(|field| field.bytes().all(|b| b.is_ascii_digit()))
}

fn day(value: &str) -> Option<u8> {
    value.parse::<u8>().ok().filter(|day| *day <= 7)
}

/// `manual`, a preset name, or `custom`.
pub fn preset(cron: Option<&str>) -> &'static str {
    match cron.map(str::trim).filter(|cron| !cron.is_empty()) {
        None => "manual",
        Some(cron) => PRESETS
            .iter()
            .find(|(_, preset_cron)| *preset_cron == cron)
            .map_or("custom", |(name, _)| name),
    }
}

/// Human label: "Manual only", "Weekdays at 9:00 PT", "Mon, Wed at 2:30 PM
/// PT", or the raw expression.
pub fn label(cron: Option<&str>) -> String {
    let preset = preset(cron);
    match preset {
        "manual" => "Manual only".to_owned(),
        "daily" => "Daily at 9:00 PT".to_owned(),
        "weekdays" => "Weekdays at 9:00 PT".to_owned(),
        "mondays" => "Mondays at 9:00 PT".to_owned(),
        "fridays" => "Fridays at 9:00 PT".to_owned(),
        _ => custom_label(cron).unwrap_or_else(|| cron.unwrap_or_default().trim().to_owned()),
    }
}

fn custom_label(cron: Option<&str>) -> Option<String> {
    let weekly = weekly(cron)?;
    let days: Vec<&str> = DAYS
        .iter()
        .filter(|(_, day)| weekly.days.contains(day))
        .map(|(label, _)| *label)
        .collect();
    let (hour, minute) = parse_time(&weekly.time)?;
    let (hour12, meridiem) = (
        if hour % 12 == 0 { 12 } else { hour % 12 },
        if hour < 12 { "AM" } else { "PM" },
    );
    Some(format!(
        "{} at {hour12}:{minute:02} {meridiem} PT",
        days.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn next_run_follows_the_time_zone() {
        let after = Utc.with_ymd_and_hms(2026, 1, 5, 12, 0, 0).unwrap();
        let next = next_run("0 9 * * *", "America/Los_Angeles", after).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 1, 5, 17, 0, 0).unwrap());
        // Strictly after: an occurrence at `after` is not returned.
        assert_eq!(
            next_run("0 9 * * *", "America/Los_Angeles", next).unwrap(),
            next + chrono::Duration::days(1)
        );
        assert_eq!(next_run("nonsense", "UTC", after), None);
        assert_eq!(next_run("0 9 * * *", "Mars/Base", after), None);
    }

    #[test]
    fn validation_and_manual_schedules() {
        assert!(is_valid("*/5 * * * *", "UTC"));
        assert!(!is_valid("61 * * * *", "UTC"));
        assert_eq!(schedule_error(None, "UTC"), None);
        assert_eq!(schedule_error(Some(" "), "UTC"), None);
        assert_eq!(schedule_error(Some("x"), "UTC"), Some(INVALID_CRON_MESSAGE));
        assert_eq!(
            next_run_if_enabled(false, Some("* * * * *"), "UTC", Utc::now()),
            None
        );
    }

    #[test]
    fn choices_presets_and_labels() {
        let days = |d: &[&str]| d.iter().map(|d| (*d).to_owned()).collect::<Vec<_>>();
        assert_eq!(cron_for_choice("manual", None, &[], None), None);
        assert_eq!(
            cron_for_choice("weekdays", None, &[], None).as_deref(),
            Some("0 9 * * 1-5")
        );
        assert_eq!(
            cron_for_choice("custom", None, &days(&["5", "1", "3"]), Some("14:30")).as_deref(),
            Some("30 14 * * 1,3,5")
        );
        let bad = cron_for_choice("custom", None, &[], Some("14:30")).unwrap();
        assert!(!is_valid(&bad, "UTC"));
        assert_eq!(preset(Some("0 9 * * 1")), "mondays");
        assert_eq!(preset(None), "manual");
        assert_eq!(preset(Some("5 4 * * 2")), "custom");
        assert_eq!(label(Some("0 9 * * 5")), "Fridays at 9:00 PT");
        assert_eq!(
            label(Some("30 14 * * 1,3,5")),
            "Mon, Wed, Fri at 2:30 PM PT"
        );
        assert_eq!(label(Some("*/5 * * * *")), "*/5 * * * *");
        let weekly = weekly(Some("0 0 * * 0,6-7")).unwrap();
        assert_eq!(weekly.days, ["6", "0"]);
        assert_eq!(weekly.time, "00:00");
        assert_eq!(label(Some("0 0 * * 1")), "Mon at 12:00 AM PT");
    }
}
