//! Console data access for the automation area: scheduled tasks.
//!
//! Covers schedule presets, labels, cron normalization and its validations, the
//! console/sandbox task payloads, the Slack delivery policy, and the parts of
//! console-user principal provisioning a task write needs.
//!
//! The write path calls the principal provisioner and the credential
//! reconciliation explicitly, in the same transaction as the write.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use time::PrimitiveDateTime;

use super::secrets::SecretKind;
use crate::{
    FieldErrors, Principal, Result,
    cron::Schedule,
    invalidate::{self, Scope},
    model::{self, ConsoleModel},
    validation::{ValidateFields, Validation, rules},
    write,
};

/// Console's default timezone for a task that does not name one.
pub const DEFAULT_TIMEZONE: &str = "America/Los_Angeles";
/// The presets the console form offers, as the cron expression each means.
pub const SCHEDULE_PRESETS: [(&str, &str); 4] = [
    ("daily", "0 9 * * *"),
    ("weekdays", "0 9 * * 1-5"),
    ("mondays", "0 9 * * 1"),
    ("fridays", "0 9 * * 5"),
];
/// The weekday names of a custom schedule, in the order the console shows them.
const CUSTOM_SCHEDULE_DAYS: [(&str, &str); 7] = [
    ("Mon", "1"),
    ("Tue", "2"),
    ("Wed", "3"),
    ("Thu", "4"),
    ("Fri", "5"),
    ("Sat", "6"),
    ("Sun", "0"),
];
/// The most channel options a task payload lists.
pub const MAX_CHANNEL_OPTIONS: i64 = 20;

/// The message for a custom schedule the user typed but cannot parse.
pub const INVALID_CUSTOM_SCHEDULE: &str = "invalid custom schedule";
const SCHEDULE_INVALID: &str = "is not a valid five-field cron schedule";
const DELIVERY_UNAVAILABLE: &str = "is not available to the author";
const NAME_TAKEN: &str = "has already been taken";
const TIMEZONE_INVALID: &str = "must be an IANA timezone name";

const TASK_COLUMNS: &str = "id, name, prompt, author_id, principal_id, delivery_channel, \
     cron_expression, timezone, enabled, next_run_at, last_enqueued_at, last_run_id, \
     last_run_at, last_error, created_at, updated_at";

/// A `scheduled_tasks` row. Timestamps are naive UTC.
#[derive(Clone, Debug, PartialEq, Eq, sqlx::FromRow)]
pub struct ScheduledTask {
    pub id: i64,
    pub name: Option<String>,
    pub prompt: String,
    pub author_id: Option<i64>,
    pub principal_id: Option<i64>,
    pub delivery_channel: String,
    pub cron_expression: Option<String>,
    pub timezone: Option<String>,
    pub enabled: bool,
    pub next_run_at: Option<PrimitiveDateTime>,
    pub last_enqueued_at: Option<PrimitiveDateTime>,
    pub last_run_id: Option<String>,
    pub last_run_at: Option<PrimitiveDateTime>,
    pub last_error: Option<String>,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

/// The task `id` when `author_id` owns it, which is Console's `owned_scope`.
pub async fn find_author_task(
    pool: &PgPool,
    author_id: i64,
    id: i64,
) -> Result<Option<ScheduledTask>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {TASK_COLUMNS} FROM {} WHERE id = $1 AND author_id = $2",
        model::ScheduledTask::TABLE
    ))
    .bind(id)
    .bind(author_id)
    .fetch_optional(pool)
    .await?)
}

/// A Console user's tasks, ordered the way the console index orders them.
pub async fn list_author_tasks(pool: &PgPool, author_id: i64) -> Result<Vec<ScheduledTask>> {
    query_tasks(
        pool,
        &format!(
            "SELECT {TASK_COLUMNS} FROM {} \
             WHERE author_id = $1 ORDER BY name ASC, id ASC",
            model::ScheduledTask::TABLE
        ),
        author_id,
    )
    .await
}

/// The tasks a Slack channel principal owns.
pub async fn list_principal_tasks(pool: &PgPool, principal_id: i64) -> Result<Vec<ScheduledTask>> {
    query_tasks(
        pool,
        &format!(
            "SELECT {TASK_COLUMNS} FROM {} \
             WHERE principal_id = $1 ORDER BY name ASC, id ASC",
            model::ScheduledTask::TABLE
        ),
        principal_id,
    )
    .await
}

async fn query_tasks(pool: &PgPool, query: &str, owner: i64) -> Result<Vec<ScheduledTask>> {
    Ok(sqlx::query_as(query).bind(owner).fetch_all(pool).await?)
}

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

/// The `#`-prefixed names of the channels `tasks` deliver to, keyed by channel
/// id.
async fn channel_names(pool: &PgPool, tasks: &[ScheduledTask]) -> Result<HashMap<String, String>> {
    let channels: Vec<String> = tasks
        .iter()
        .map(|task| task.delivery_channel.clone())
        .filter(|channel| !channel.is_empty())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let mut names = HashMap::new();
    if channels.is_empty() {
        return Ok(names);
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT channel_id, name FROM slack_bot_channels WHERE channel_id = ANY($1)",
    )
    .bind(&channels)
    .fetch_all(pool)
    .await?;
    for (channel_id, name) in rows {
        names.insert(channel_id, format!("#{name}"));
    }
    Ok(names)
}

/// The console payloads for `tasks`: `slack_channel_names` plus the author's
/// direct message id, resolved once per author.
pub async fn console_payloads(
    pool: &PgPool,
    tasks: &[ScheduledTask],
) -> Result<Vec<ConsoleTaskPayload>> {
    let names = channel_names(pool, tasks).await?;
    let mut direct_messages: HashMap<i64, Option<String>> = HashMap::new();
    let mut payloads = Vec::with_capacity(tasks.len());
    for task in tasks {
        let direct_message = match task.author_id {
            Some(author_id) => match direct_messages.get(&author_id) {
                Some(direct_message) => direct_message.clone(),
                None => {
                    let direct_message =
                        slack_policy_for_user(pool, author_id).await?.slack_user_id;
                    direct_messages.insert(author_id, direct_message.clone());
                    direct_message
                }
            },
            None => None,
        };
        payloads.push(ConsoleTaskPayload::new(
            task,
            &names,
            direct_message.as_deref(),
        ));
    }
    Ok(payloads)
}

/// The console payload for one task, resolving the delivery names and the
/// author's direct message id the way the console view does.
pub async fn console_payload_for(
    pool: &PgPool,
    task: &ScheduledTask,
) -> Result<ConsoleTaskPayload> {
    let names = channel_names(pool, std::slice::from_ref(task)).await?;
    let direct_message = match task.author_id {
        Some(author_id) => slack_policy_for_user(pool, author_id).await?.slack_user_id,
        None => None,
    };
    Ok(ConsoleTaskPayload::new(
        task,
        &names,
        direct_message.as_deref(),
    ))
}

/// `ScheduledTaskConsoleAttributes`' task payload: the stored row plus the
/// delivery names and the author's direct message id the console form shows.
#[derive(Debug, Serialize)]
pub struct ConsoleTaskPayload {
    pub id: String,
    pub name: Option<String>,
    pub prompt: String,
    pub author_id: Option<String>,
    pub principal_id: Option<String>,
    pub delivery_channel: Option<String>,
    pub delivery_mode: &'static str,
    pub delivery_name: Option<String>,
    pub direct_message_user_id: Option<String>,
    pub cron_expression: Option<String>,
    pub timezone: String,
    pub schedule_preset: &'static str,
    pub schedule_label: String,
    pub custom_days: Vec<String>,
    pub custom_time: String,
    pub enabled: bool,
    #[serde(with = "crate::timestamp::primitive_option")]
    pub next_run_at: Option<PrimitiveDateTime>,
    #[serde(with = "crate::timestamp::primitive_option")]
    pub last_enqueued_at: Option<PrimitiveDateTime>,
    pub last_run_id: Option<String>,
    #[serde(with = "crate::timestamp::primitive_option")]
    pub last_run_at: Option<PrimitiveDateTime>,
    pub last_error: Option<String>,
    #[serde(with = "crate::timestamp::primitive")]
    pub created_at: PrimitiveDateTime,
    #[serde(with = "crate::timestamp::primitive")]
    pub updated_at: PrimitiveDateTime,
}

impl ConsoleTaskPayload {
    fn new(
        task: &ScheduledTask,
        names: &HashMap<String, String>,
        direct_message: Option<&str>,
    ) -> Self {
        let delivers_to_dm = !task.delivery_channel.is_empty()
            && direct_message == Some(task.delivery_channel.as_str());
        Self {
            id: model::ScheduledTask::oid(task.id),
            name: task.name.clone(),
            prompt: task.prompt.clone(),
            author_id: task.author_id.map(model::User::oid),
            principal_id: task.principal_id.map(model::Principal::oid),
            delivery_channel: (!task.delivery_channel.is_empty())
                .then(|| task.delivery_channel.clone()),
            delivery_mode: delivery_mode(task, direct_message),
            delivery_name: if delivers_to_dm {
                Some("Direct message to you".to_owned())
            } else {
                names.get(&task.delivery_channel).cloned()
            },
            direct_message_user_id: direct_message.map(str::to_owned),
            cron_expression: task.cron_expression.clone(),
            timezone: task
                .timezone
                .clone()
                .unwrap_or_else(|| DEFAULT_TIMEZONE.to_owned()),
            schedule_preset: task.schedule_preset(),
            schedule_label: task.schedule_label(),
            custom_days: task.custom_schedule_days(),
            custom_time: task.custom_schedule_time(),
            enabled: task.enabled,
            next_run_at: task.next_run_at,
            last_enqueued_at: task.last_enqueued_at,
            last_run_id: task.last_run_id.clone(),
            last_run_at: task.last_run_at,
            last_error: task.last_error.clone(),
            created_at: task.created_at,
            updated_at: task.updated_at,
        }
    }
}

/// `new`'s form defaults: an unsaved task with the daily preset, enabled, and
/// the caller's Slack DM as the destination when one is linked.
pub fn new_task_payload(user_oid: &str, direct_message: Option<&str>) -> NewTaskPayload {
    let cron = SCHEDULE_PRESETS[0].1;
    let (custom_days, custom_time) =
        simple_weekly(cron).unwrap_or_else(|| (Vec::new(), "09:00".to_owned()));
    NewTaskPayload {
        id: None,
        name: None,
        prompt: None,
        author_id: user_oid.to_owned(),
        principal_id: None,
        delivery_channel: None,
        delivery_mode: if direct_message.is_some() {
            "dm"
        } else {
            "channel"
        },
        delivery_name: None,
        direct_message_user_id: direct_message.map(str::to_owned),
        cron_expression: cron.to_owned(),
        timezone: DEFAULT_TIMEZONE.to_owned(),
        schedule_preset: schedule_preset_for(Some(cron)),
        schedule_label: schedule_label(Some(cron)),
        custom_days,
        custom_time,
        enabled: true,
        next_run_at: None,
        last_enqueued_at: None,
        last_run_id: None,
        last_run_at: None,
        last_error: None,
        created_at: None,
        updated_at: None,
    }
}

/// The unsaved task the console's `new` form renders.
#[derive(Debug, Serialize)]
pub struct NewTaskPayload {
    pub id: Option<String>,
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub author_id: String,
    pub principal_id: Option<String>,
    pub delivery_channel: Option<String>,
    pub delivery_mode: &'static str,
    pub delivery_name: Option<String>,
    pub direct_message_user_id: Option<String>,
    pub cron_expression: String,
    pub timezone: String,
    pub schedule_preset: &'static str,
    pub schedule_label: String,
    pub custom_days: Vec<String>,
    pub custom_time: String,
    pub enabled: bool,
    #[serde(with = "crate::timestamp::primitive_option")]
    pub next_run_at: Option<PrimitiveDateTime>,
    #[serde(with = "crate::timestamp::primitive_option")]
    pub last_enqueued_at: Option<PrimitiveDateTime>,
    pub last_run_id: Option<String>,
    #[serde(with = "crate::timestamp::primitive_option")]
    pub last_run_at: Option<PrimitiveDateTime>,
    pub last_error: Option<String>,
    #[serde(with = "crate::timestamp::primitive_option")]
    pub created_at: Option<PrimitiveDateTime>,
    #[serde(with = "crate::timestamp::primitive_option")]
    pub updated_at: Option<PrimitiveDateTime>,
}

/// A user id destination is a DM, any other destination a channel; a task
/// without a destination defaults to the DM when one is linked.
fn delivery_mode(task: &ScheduledTask, direct_message: Option<&str>) -> &'static str {
    if task.delivery_channel.starts_with(['U', 'W']) {
        return "dm";
    }
    if !task.delivery_channel.is_empty() {
        return "channel";
    }
    if direct_message.is_some() {
        "dm"
    } else {
        "channel"
    }
}

/// The sandbox payload for a list of tasks, in the order given.
pub fn sandbox_payloads(tasks: &[ScheduledTask]) -> Vec<SandboxTaskPayload> {
    tasks.iter().map(sandbox_payload).collect()
}

/// One task in a Slack channel principal's scope.
pub async fn find_principal_task(
    pool: &PgPool,
    principal_id: i64,
    id: i64,
) -> Result<Option<ScheduledTask>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {TASK_COLUMNS} FROM {} WHERE id = $1 AND principal_id = $2",
        model::ScheduledTask::TABLE
    ))
    .bind(id)
    .bind(principal_id)
    .fetch_optional(pool)
    .await?)
}

/// The principal that sent the turn a proxy is acting in.
pub async fn proxy_requester(pool: &PgPool, proxy_id: i64) -> Result<Option<Principal>> {
    let principal_id: Option<i64> = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT requester_principal_id FROM proxies WHERE id = $1",
    )
    .bind(proxy_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    match principal_id {
        Some(principal_id) => crate::principals::find_principal(pool, principal_id).await,
        None => Ok(None),
    }
}

/// The sandbox payload: the execution state a sandbox session sees.
#[derive(Debug, Serialize)]
pub struct SandboxTaskPayload {
    pub id: String,
    pub name: Option<String>,
    pub prompt: String,
    pub delivery_channel: String,
    pub cron_expression: Option<String>,
    pub timezone: Option<String>,
    pub schedule_label: String,
    pub enabled: bool,
    #[serde(with = "crate::timestamp::primitive_option")]
    pub next_run_at: Option<PrimitiveDateTime>,
    #[serde(with = "crate::timestamp::primitive_option")]
    pub last_enqueued_at: Option<PrimitiveDateTime>,
    pub last_run_id: Option<String>,
    #[serde(with = "crate::timestamp::primitive_option")]
    pub last_run_at: Option<PrimitiveDateTime>,
    pub last_error: Option<String>,
    #[serde(with = "crate::timestamp::primitive")]
    pub created_at: PrimitiveDateTime,
    #[serde(with = "crate::timestamp::primitive")]
    pub updated_at: PrimitiveDateTime,
}

impl From<&ScheduledTask> for SandboxTaskPayload {
    fn from(task: &ScheduledTask) -> Self {
        Self {
            id: model::ScheduledTask::oid(task.id),
            name: task.name.clone(),
            prompt: task.prompt.clone(),
            delivery_channel: task.delivery_channel.clone(),
            cron_expression: task.cron_expression.clone(),
            timezone: task.timezone.clone(),
            schedule_label: schedule_label(task.cron_expression.as_deref()),
            enabled: task.enabled,
            next_run_at: task.next_run_at,
            last_enqueued_at: task.last_enqueued_at,
            last_run_id: task.last_run_id.clone(),
            last_run_at: task.last_run_at,
            last_error: task.last_error.clone(),
            created_at: task.created_at,
            updated_at: task.updated_at,
        }
    }
}

pub fn sandbox_payload(task: &ScheduledTask) -> SandboxTaskPayload {
    SandboxTaskPayload::from(task)
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

/// The preset a stored expression matches, otherwise `custom`, otherwise
/// `manual` for a task without a cron expression.
pub fn schedule_preset_for(cron_expression: Option<&str>) -> &'static str {
    let Some(cron) = cron_expression.filter(|cron| !cron.is_empty()) else {
        return "manual";
    };
    SCHEDULE_PRESETS
        .iter()
        .find(|(_, expression)| *expression == cron)
        .map_or("custom", |(preset, _)| preset)
}

/// The human label of a stored cron expression.
pub fn schedule_label(cron_expression: Option<&str>) -> String {
    let Some(cron) = cron_expression.filter(|cron| !cron.is_empty()) else {
        return "Manual only".to_owned();
    };
    match schedule_preset_for(Some(cron)) {
        "daily" => "Daily at 9:00 PT".to_owned(),
        "weekdays" => "Weekdays at 9:00 PT".to_owned(),
        "mondays" => "Mondays at 9:00 PT".to_owned(),
        "fridays" => "Fridays at 9:00 PT".to_owned(),
        _ => custom_schedule_label(cron).unwrap_or_else(|| cron.to_owned()),
    }
}

/// The days (as Console stores them) and the time of day a weekly cron
/// expression means.
fn simple_weekly(cron: &str) -> Option<(Vec<String>, String)> {
    let fields: Vec<&str> = cron.split_whitespace().collect();
    let [minute, hour, monthdays, months, weekdays] = fields[..] else {
        return None;
    };
    if monthdays != "*" || months != "*" {
        return None;
    }
    let minute = parse_number(minute, 59)?;
    let hour = parse_number(hour, 23)?;
    let days = expand_schedule_days(weekdays);
    if days.is_empty() {
        return None;
    }
    Some((days, format!("{hour:02}:{minute:02}")))
}

fn parse_number(field: &str, max: u8) -> Option<u8> {
    if field.is_empty() || !field.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u8 = field.parse().ok()?;
    (value <= max).then_some(value)
}

/// The selected days as Console's day values, in the order the console form
/// shows them.
fn expand_schedule_days(day_expression: &str) -> Vec<String> {
    let selected: Vec<String> = if day_expression == "*" {
        return CUSTOM_SCHEDULE_DAYS
            .iter()
            .map(|(_, value)| (*value).to_owned())
            .collect();
    } else {
        let mut selected = Vec::new();
        for part in day_expression.split(',') {
            let bytes = part.as_bytes();
            if bytes.len() == 3
                && bytes[1] == b'-'
                && is_day_digit(bytes[0])
                && is_day_digit(bytes[2])
            {
                for day in bytes[0]..=bytes[2] {
                    selected.push(day_value(day));
                }
            } else if bytes.len() == 1 && is_day_digit(bytes[0]) {
                selected.push(day_value(bytes[0]));
            }
        }
        selected
    };
    CUSTOM_SCHEDULE_DAYS
        .iter()
        .filter(|(_, value)| selected.iter().any(|day| day == value))
        .map(|(_, value)| (*value).to_owned())
        .collect()
}

fn is_day_digit(byte: u8) -> bool {
    (b'0'..=b'7').contains(&byte)
}

/// Cron's `7` and `0` both mean Sunday.
fn day_value(day: u8) -> String {
    let day = if day == b'7' { b'0' } else { day };
    (day as char).to_string()
}

/// The custom label, e.g. "Mon, Wed at 2:30 PM PT".
fn custom_schedule_label(cron: &str) -> Option<String> {
    let (days, time) = simple_weekly(cron)?;
    let labels = CUSTOM_SCHEDULE_DAYS
        .iter()
        .filter(|(_, value)| days.iter().any(|day| day == value))
        .map(|(label, _)| *label)
        .collect::<Vec<_>>()
        .join(", ");
    let (hour, minute) = time.split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    let meridiem = if hour < 12 { "AM" } else { "PM" };
    let display_hour = match hour % 12 {
        0 => 12,
        hour => hour,
    };
    Some(format!(
        "{labels} at {display_hour}:{minute:02} {meridiem} PT"
    ))
}

impl ScheduledTask {
    /// The days a custom expression selects.
    pub fn custom_schedule_days(&self) -> Vec<String> {
        self.cron_expression
            .as_deref()
            .and_then(simple_weekly)
            .map(|(days, _)| days)
            .unwrap_or_default()
    }

    /// The time of day a custom expression selects.
    pub fn custom_schedule_time(&self) -> String {
        self.cron_expression
            .as_deref()
            .and_then(simple_weekly)
            .map(|(_, time)| time)
            .unwrap_or_else(|| "09:00".to_owned())
    }

    /// The preset this task's expression matches.
    pub fn schedule_preset(&self) -> &'static str {
        schedule_preset_for(self.cron_expression.as_deref())
    }

    /// The human label of this task's expression.
    pub fn schedule_label(&self) -> String {
        schedule_label(self.cron_expression.as_deref())
    }
}

/// The cron expression a console submission asks for.
pub fn cron_for(
    preset: &str,
    custom_expression: Option<&str>,
    custom_days: &[String],
    custom_time: Option<&str>,
) -> Option<String> {
    match preset {
        "manual" => None,
        "cron" => Some(custom_expression.unwrap_or_default().trim().to_owned()),
        "custom" => Some(custom_cron(custom_days, custom_time)),
        _ => SCHEDULE_PRESETS
            .iter()
            .find(|(name, _)| *name == preset)
            .map(|(_, expression)| (*expression).to_owned())
            .or_else(|| Some(custom_expression.unwrap_or_default().trim().to_owned())),
    }
}

/// `ScheduledTask.custom_cron`: weekdays and a time as a cron expression, or
/// an empty expression when the input cannot express one.
fn custom_cron(days: &[String], time: Option<&str>) -> String {
    let selected: Vec<&str> = days.iter().map(String::as_str).collect();
    let ordered_days: Vec<&str> = CUSTOM_SCHEDULE_DAYS
        .iter()
        .filter(|(_, value)| selected.contains(value))
        .map(|(_, value)| *value)
        .collect();
    let time = time.unwrap_or_default().as_bytes();
    if ordered_days.is_empty()
        || time.len() != 5
        || time[2] != b':'
        || !time[0].is_ascii_digit()
        || !time[1].is_ascii_digit()
        || !time[3].is_ascii_digit()
        || !time[4].is_ascii_digit()
    {
        return String::new();
    }
    let hour = u32::from(time[0] - b'0') * 10 + u32::from(time[1] - b'0');
    let minute = u32::from(time[3] - b'0') * 10 + u32::from(time[4] - b'0');
    if hour > 23 || minute > 59 {
        return String::new();
    }
    format!("{minute} {hour} * * {}", ordered_days.join(","))
}

/// `ScheduledTask.cron_schedule_is_valid`: a schedule api-rs can step through.
/// [`Schedule::parse`] already reads the same five-field grammar, and more
/// strictly, so its answer is the validation.
pub fn cron_valid(expression: &str, timezone: &str) -> bool {
    Schedule::parse(expression, timezone).is_ok()
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// The task fields a console or sandbox submission changes. `None` keeps the
/// stored value; `Some(None)` clears the column.
#[derive(Clone, Debug, Default)]
pub struct TaskDraft {
    pub name: Option<String>,
    pub prompt: Option<String>,
    pub enabled: Option<bool>,
    pub delivery_channel: Option<Option<String>>,
    pub cron_expression: Option<Option<String>>,
}

/// Who owns the task being written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskOwner {
    /// A Console user's own task.
    Author(i64),
    /// A Slack channel principal's task.
    Channel(i64),
}

/// The outcome of a task write: the stored row, or the model's validation
/// errors keyed by attribute.
#[derive(Clone, Debug)]
pub struct TaskWrite {
    pub task: Option<ScheduledTask>,
    pub errors: FieldErrors,
}

impl TaskWrite {
    fn invalid(errors: FieldErrors) -> Self {
        Self { task: None, errors }
    }

    fn saved(task: ScheduledTask) -> Self {
        Self {
            task: Some(task),
            errors: FieldErrors::new(),
        }
    }
}

/// The catalog-loading or team-mismatch error a task write reports, if any.
pub async fn catalog_error(pool: &PgPool, policy: &SlackPolicy) -> Result<Option<String>> {
    if let Some(error) = super::principals::slack_catalog_error(pool).await? {
        return Ok(Some(error.to_owned()));
    }
    let Some(team_id) = policy.slack_team_id.as_deref() else {
        return Ok(None);
    };
    let loading: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM slack_bot_channels \
         WHERE active = true AND archived = false AND team_id = $1 AND private = true \
         AND membership_refreshed_at IS NULL)",
    )
    .bind(team_id)
    .fetch_one(pool)
    .await?;
    Ok(loading.then(|| "Slack channel memberships are loading.".to_owned()))
}

/// One console delivery option: `slack_channel_option`'s payload.
pub async fn slack_channel_options(
    pool: &PgPool,
    policy: &SlackPolicy,
    query: Option<&str>,
    limit: i64,
) -> Result<Vec<ChannelOption>> {
    let needle = query.unwrap_or_default().trim();
    let matching = format!(
        "%{}%",
        needle
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let needle = if needle.is_empty() {
        None
    } else {
        Some(matching)
    };
    let rows: Vec<ChannelOptionRow> = sqlx::query_as(
        "SELECT channel_id, name, private FROM slack_bot_channels \
         WHERE active = true AND archived = false \
           AND ($1::text IS NULL OR team_id = $1) \
           AND (private = false OR ($2::text IS NOT NULL AND member_user_ids @> ARRAY[ \
                 (SELECT bot_user_id FROM slack_bot_channels LIMIT 1), $2 ]::text[])) \
           AND ($3::text IS NULL OR name ILIKE $3 OR channel_id ILIKE $3) \
         ORDER BY lower(name) ASC, channel_id ASC \
         LIMIT $4",
    )
    .bind(policy.slack_team_id.as_deref())
    .bind(policy.slack_user_id.as_deref())
    .bind(needle)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(ChannelOption::from).collect())
}

/// One selector option of the console's channel picker.
#[derive(Debug, Serialize)]
pub struct ChannelOption {
    pub channel_id: String,
    pub name: String,
    pub private: bool,
    pub label: String,
    pub description: String,
}

impl From<&ChannelOptionRow> for ChannelOption {
    fn from(row: &ChannelOptionRow) -> Self {
        let state = if row.private { "Private" } else { "Public" };
        Self {
            channel_id: row.channel_id.clone(),
            name: row.name.clone(),
            private: row.private,
            label: format!("#{}", row.name),
            description: format!("{} · {state}", row.channel_id),
        }
    }
}

/// The columns [`slack_channel_options`] reads per option.
#[derive(Debug, sqlx::FromRow)]
struct ChannelOptionRow {
    channel_id: String,
    name: String,
    private: bool,
}

// ---------------------------------------------------------------------------
// Slack delivery policy
// ---------------------------------------------------------------------------

/// `SlackDeliveryPolicy`'s answer to "which Slack identity does this user
/// speak as", which decides the direct message destination and the channels
/// they may deliver to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlackPolicy {
    pub slack_user_id: Option<String>,
    pub slack_team_id: Option<String>,
}

/// `SlackDeliveryPolicy.new(user)`, from the user's Slack identities, the
/// Console principals linked to the user, and their Slack credentials.
pub async fn slack_policy_for_user(pool: &PgPool, user_id: i64) -> Result<SlackPolicy> {
    let mut connection = pool.acquire().await?;
    slack_policy(&mut connection, user_id).await
}

pub(crate) async fn slack_policy(
    connection: &mut PgConnection,
    user_id: i64,
) -> Result<SlackPolicy> {
    let mut candidates: Vec<(String, String)> = Vec::new();
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT subject, team_id FROM user_identities \
         WHERE user_id = $1 AND provider = 'slack' ORDER BY id ASC",
    )
    .bind(user_id)
    .fetch_all(&mut *connection)
    .await?;
    for (subject, team_id) in rows {
        candidates.push((subject, team_id.unwrap_or_default()));
    }
    let rows: Vec<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT slack_user_id, slack_team_id FROM principals \
         WHERE kind = 'console_user' AND console_user_id = $1",
    )
    .bind(user_id)
    .fetch_all(&mut *connection)
    .await?;
    for (subject, team_id) in rows {
        candidates.push((subject.unwrap_or_default(), team_id.unwrap_or_default()));
    }
    let rows: Vec<(Option<String>, Value, Value)> = sqlx::query_as(
        "SELECT broker_credentials.provider_subject, broker_credentials.labels, \
                oauth_apps.labels AS app_labels \
         FROM broker_credentials \
         JOIN oauth_apps ON oauth_apps.id = broker_credentials.oauth_app_id \
         WHERE broker_credentials.created_by_id = $1 AND oauth_apps.provider = 'slack'",
    )
    .bind(user_id)
    .fetch_all(&mut *connection)
    .await?;
    for (subject, labels, app_labels) in rows {
        let team_id = labels
            .get("slack_team_id")
            .and_then(Value::as_str)
            .filter(|team| !team.is_empty())
            .or_else(|| {
                app_labels
                    .get("slack_team_id")
                    .and_then(Value::as_str)
                    .filter(|team| !team.is_empty())
            })
            .unwrap_or_default();
        candidates.push((subject.unwrap_or_default(), team_id.to_owned()));
    }
    let mut valid: Vec<(String, String)> = Vec::new();
    for (subject, team_id) in candidates {
        if !slack_user_id_valid(&subject) || !slack_team_id_valid(&team_id) {
            continue;
        }
        if !valid.contains(&(subject.clone(), team_id.clone())) {
            valid.push((subject, team_id));
        }
    }
    Ok(match valid.as_slice() {
        [identity] => SlackPolicy {
            slack_user_id: Some(identity.0.clone()),
            slack_team_id: Some(identity.1.clone()),
        },
        _ => SlackPolicy::default(),
    })
}

/// The Slack id format shared by user ids, team ids, and delivery
/// destinations: one of `prefixes`, then uppercase alphanumerics, at least nine
/// characters in all.
pub(crate) fn slack_id_valid(value: &str, prefixes: &[u8]) -> bool {
    let bytes = value.as_bytes();
    bytes.first().is_some_and(|first| prefixes.contains(first))
        && bytes.len() >= 9
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// The user-id format; `USLACK` is Console's bot identity.
pub(crate) fn slack_user_id_valid(value: &str) -> bool {
    value == "USLACK" || slack_id_valid(value, b"UW")
}

/// The team-id format.
pub(crate) fn slack_team_id_valid(value: &str) -> bool {
    slack_id_valid(value, b"TE")
}

/// The delivery-destination format.
fn delivery_destination_valid(value: &str) -> bool {
    slack_id_valid(value, b"CDGUW")
}

/// Whether the destination is one the policy allows.
async fn delivery_destination_available(
    connection: &mut PgConnection,
    owner: TaskOwner,
    destination: &str,
) -> Result<bool> {
    let destination = destination.trim().to_uppercase();
    match owner {
        TaskOwner::Author(user_id) => {
            let policy = slack_policy(connection, user_id).await?;
            if policy.slack_user_id.as_deref() == Some(destination.as_str()) {
                return Ok(true);
            }
            let found: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM slack_bot_channels \
                 WHERE active = true AND archived = false \
                   AND ($1::text IS NULL OR team_id = $1) \
                   AND channel_id = $2 \
                   AND (private = false OR ($3::text IS NOT NULL AND member_user_ids @> ARRAY[ \
                         (SELECT bot_user_id FROM slack_bot_channels LIMIT 1), $3 ]::text[])))",
            )
            .bind(policy.slack_team_id.as_deref())
            .bind(&destination)
            .bind(policy.slack_user_id.as_deref())
            .fetch_one(&mut *connection)
            .await?;
            Ok(found)
        }
        TaskOwner::Channel(principal_id) => {
            let team_id: Option<String> = sqlx::query_scalar::<_, Option<String>>(
                "SELECT slack_team_id FROM principals WHERE id = $1",
            )
            .bind(principal_id)
            .fetch_optional(&mut *connection)
            .await?
            .flatten();
            let found: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM slack_bot_channels \
                 WHERE active = true AND archived = false \
                   AND ($1::text IS NULL OR team_id = $1) AND channel_id = $2)",
            )
            .bind(team_id)
            .bind(&destination)
            .fetch_one(&mut *connection)
            .await?;
            Ok(found)
        }
    }
}

// ---------------------------------------------------------------------------
// Principal provisioning
// ---------------------------------------------------------------------------

/// The Console principal a task's author runs as, provisioned through the same
/// path the MCP consent flow uses.
pub async fn provision_author_principal(pool: &PgPool, user_id: i64) -> Result<Option<i64>> {
    Ok(
        super::auth_pages::provision_console_user_principal(pool, user_id)
            .await?
            .map(|principal| principal.id),
    )
}

/// The label set a provisioned console user's principal carries: the repo-cache
/// label plus the managed-by marker.
pub(crate) fn principal_labels(labels: &Value, repo_cache: &str) -> Value {
    let mut labels = labels.as_object().cloned().unwrap_or_default();
    labels.insert("managed-by".to_owned(), Value::String("centaur".to_owned()));
    labels.insert(
        "centaur.sandbox_repo_cache".to_owned(),
        Value::String(repo_cache.to_owned()),
    );
    Value::Object(labels)
}

// ---------------------------------------------------------------------------
// Task writes
// ---------------------------------------------------------------------------

/// Creates a task for `owner` from a console or sandbox submission.
pub async fn create_task(pool: &PgPool, owner: TaskOwner, draft: TaskDraft) -> Result<TaskWrite> {
    Ok(save_task(pool, owner, None, draft)
        .await?
        .unwrap_or_else(|| TaskWrite::invalid(FieldErrors::new())))
}

/// Updates a task `owner` owns, or `None` when the task is not in the owner's
/// scope (Console's 404).
pub async fn update_task(
    pool: &PgPool,
    owner: TaskOwner,
    id: i64,
    draft: TaskDraft,
) -> Result<Option<TaskWrite>> {
    save_task(pool, owner, Some(id), draft).await
}

/// Deletes the task, and nothing else: the owner's scope is part of the delete,
/// so another owner's task is not found.
pub async fn delete_task(pool: &PgPool, owner: TaskOwner, id: i64) -> Result<bool> {
    let mut tx = begin_write(pool).await?;
    let deleted = match owner {
        TaskOwner::Author(author_id) => {
            sqlx::query(&format!(
                "DELETE FROM {} WHERE id = $1 AND author_id = $2",
                model::ScheduledTask::TABLE
            ))
            .bind(id)
            .bind(author_id)
            .execute(&mut *tx)
            .await?
        }
        TaskOwner::Channel(principal_id) => {
            sqlx::query(&format!(
                "DELETE FROM {} WHERE id = $1 AND principal_id = $2",
                model::ScheduledTask::TABLE
            ))
            .bind(id)
            .bind(principal_id)
            .execute(&mut *tx)
            .await?
        }
    };
    tx.commit().await?;
    Ok(deleted.rows_affected() > 0)
}

async fn save_task(
    pool: &PgPool,
    owner: TaskOwner,
    id: Option<i64>,
    draft: TaskDraft,
) -> Result<Option<TaskWrite>> {
    let mut tx = begin_write(pool).await?;
    let existing = match id {
        Some(id) => {
            let row: Option<ScheduledTask> = match owner {
                TaskOwner::Author(author_id) => {
                    sqlx::query_as(&format!(
                        "SELECT {TASK_COLUMNS} FROM {} \
                     WHERE id = $1 AND author_id = $2 FOR UPDATE",
                        model::ScheduledTask::TABLE
                    ))
                    .bind(id)
                    .bind(author_id)
                    .fetch_optional(&mut *tx)
                    .await?
                }
                TaskOwner::Channel(principal_id) => {
                    sqlx::query_as(&format!(
                        "SELECT {TASK_COLUMNS} FROM {} \
                     WHERE id = $1 AND principal_id = $2 FOR UPDATE",
                        model::ScheduledTask::TABLE
                    ))
                    .bind(id)
                    .bind(principal_id)
                    .fetch_optional(&mut *tx)
                    .await?
                }
            };
            match row {
                Some(row) => Some(row),
                None => return Ok(None),
            }
        }
        None => None,
    };

    let mut candidate = Candidate::from(existing.clone());
    if let Some(name) = draft.name {
        candidate.name = Some(name.trim().to_owned());
    }
    if let Some(prompt) = draft.prompt {
        candidate.prompt = prompt.replace("\r\n", "\n").trim().to_owned();
    }
    if let Some(enabled) = draft.enabled {
        candidate.enabled = enabled;
    }
    if let Some(delivery_channel) = draft.delivery_channel {
        candidate.delivery_channel = delivery_channel.unwrap_or_default().trim().to_owned();
    }
    if let Some(cron_expression) = draft.cron_expression {
        candidate.cron_expression = cron_expression
            .map(|cron| cron.trim().to_owned())
            .filter(|cron| !cron.is_empty());
    }
    // `before_validation :set_channel_delivery, if: :principal`
    if let TaskOwner::Channel(principal_id) = owner {
        let channel: Option<String> =
            sqlx::query_scalar("SELECT slack_channel_id FROM principals WHERE id = $1")
                .bind(principal_id)
                .fetch_optional(&mut *tx)
                .await?
                .flatten();
        candidate.delivery_channel = channel.unwrap_or_default().trim().to_owned();
    }
    // `before_validation :set_default_timezone`
    if candidate
        .timezone
        .as_deref()
        .is_none_or(|timezone| timezone.trim().is_empty())
    {
        candidate.timezone = Some(DEFAULT_TIMEZONE.to_owned());
    }
    // `before_validation :refresh_next_run_at, if: :schedule_requires_refresh?`
    let refresh = !candidate.enabled
        || candidate.next_run_at.is_none()
        || existing
            .as_ref()
            .is_none_or(|existing| existing.enabled != candidate.enabled)
        || existing
            .as_ref()
            .map(|existing| existing.cron_expression.as_ref())
            != Some(candidate.cron_expression.as_ref())
        || existing.as_ref().map(|existing| existing.timezone.as_ref())
            != Some(candidate.timezone.as_ref());
    if refresh {
        candidate.next_run_at = if candidate.enabled {
            candidate
                .cron_expression
                .clone()
                .zip(candidate.timezone.clone())
                .and_then(|(cron, timezone)| {
                    let now = chrono::Utc::now();
                    Schedule::parse(&cron, &timezone)
                        .ok()
                        .and_then(|schedule| schedule.next_after(now))
                        .map(|time| crate::timestamp::datetime(crate::timestamp::from_chrono(time)))
                })
        } else {
            None
        };
    }

    let errors = validate(
        &mut tx,
        owner,
        &candidate,
        existing.as_ref().map(|task| task.id),
    )
    .await?;
    if !errors.is_empty() {
        return Ok(Some(TaskWrite::invalid(errors)));
    }

    let changed = match &existing {
        Some(existing) => {
            existing.name != candidate.name
                || existing.prompt != candidate.prompt
                || existing.delivery_channel != candidate.delivery_channel
                || existing.cron_expression != candidate.cron_expression
                || existing.timezone != candidate.timezone
                || existing.enabled != candidate.enabled
                || existing.next_run_at != candidate.next_run_at
        }
        None => true,
    };
    let mut wrote = false;
    let stored = match &existing {
        Some(existing) if !changed => existing.clone(),
        Some(existing) => {
            wrote = true;
            // The owner's scope selected the row, and `FOR UPDATE` holds it, so
            // the update only needs the id. `changed` above is the dirty check:
            // an unchanged write never reaches this statement.
            let sql = format!(
                "{} RETURNING {TASK_COLUMNS}",
                write::update_sql(
                    model::ScheduledTask::TABLE,
                    &[
                        "name",
                        "prompt",
                        "delivery_channel",
                        "cron_expression",
                        "timezone",
                        "enabled",
                        "next_run_at",
                    ],
                )
            );
            sqlx::query_as::<_, ScheduledTask>(&sql)
                .bind(&candidate.name)
                .bind(&candidate.prompt)
                .bind(&candidate.delivery_channel)
                .bind(&candidate.cron_expression)
                .bind(&candidate.timezone)
                .bind(candidate.enabled)
                .bind(candidate.next_run_at)
                .bind(existing.id)
                .fetch_one(&mut *tx)
                .await?
        }
        None => {
            wrote = true;
            let (author_id, principal_id) = match owner {
                TaskOwner::Author(author_id) => (Some(author_id), None),
                TaskOwner::Channel(principal_id) => (None, Some(principal_id)),
            };
            let sql = write::insert_returning_sql(
                model::ScheduledTask::TABLE,
                &[
                    "author_id",
                    "principal_id",
                    "name",
                    "prompt",
                    "delivery_channel",
                    "cron_expression",
                    "timezone",
                    "enabled",
                    "next_run_at",
                ],
                TASK_COLUMNS,
            );
            sqlx::query_as::<_, ScheduledTask>(&sql)
                .bind(author_id)
                .bind(principal_id)
                .bind(&candidate.name)
                .bind(&candidate.prompt)
                .bind(&candidate.delivery_channel)
                .bind(&candidate.cron_expression)
                .bind(&candidate.timezone)
                .bind(candidate.enabled)
                .bind(candidate.next_run_at)
                .fetch_one(&mut *tx)
                .await?
        }
    };
    tx.commit().await?;
    // Provision the author's principal after the commit, on create or update.
    if wrote && let TaskOwner::Author(author_id) = owner {
        provision_author_principal(pool, author_id).await?;
    }
    Ok(Some(TaskWrite::saved(stored)))
}

/// The attributes the model holds while it validates.
///
/// This is a merged view of the stored row and the submitted draft, because
/// Console validates the record it is about to write, not the request alone.
/// Every rule that only reads the record is a garde attribute; the two that
/// need the database (name uniqueness, destination availability) are in
/// [`validate`].
#[derive(Debug, garde::Validate)]
struct Candidate {
    // A present name of at most 100 characters.
    #[garde(custom(rules::present_not_blank), length(chars, max = 100))]
    name: Option<String>,
    // A present prompt of at most 64 KiB.
    #[garde(length(chars, min = 1, max = 65536))]
    prompt: String,
    // Present, and a Slack channel or user id.
    #[garde(length(chars, min = 1), pattern(r"^[CDGUW][A-Z0-9]{8,}$"))]
    delivery_channel: String,
    // Valid cron, read in the record's timezone.
    #[garde(custom(task_rules::cron_schedule(self.timezone.as_deref())))]
    cron_expression: Option<String>,
    // Present, and an IANA timezone name.
    #[garde(
        custom(rules::present_not_blank),
        custom(task_rules::timezone_included)
    )]
    timezone: Option<String>,
    #[garde(skip)]
    enabled: bool,
    #[garde(skip)]
    next_run_at: Option<PrimitiveDateTime>,
}

impl From<Option<ScheduledTask>> for Candidate {
    fn from(task: Option<ScheduledTask>) -> Self {
        match task {
            Some(task) => Self {
                name: task.name,
                prompt: task.prompt,
                delivery_channel: task.delivery_channel,
                cron_expression: task.cron_expression,
                timezone: task.timezone,
                enabled: task.enabled,
                next_run_at: task.next_run_at,
            },
            None => Self {
                name: None,
                prompt: String::new(),
                delivery_channel: String::new(),
                cron_expression: None,
                timezone: None,
                enabled: true,
                next_run_at: None,
            },
        }
    }
}

/// The field rules garde has no built-in for.
mod task_rules {
    use super::{SCHEDULE_INVALID, TIMEZONE_INVALID, cron_valid};

    /// The timezone must be an IANA identifier.
    pub(super) fn timezone_included(value: &Option<String>, _: &()) -> garde::Result {
        match value.as_deref() {
            Some(timezone)
                if !timezone.is_empty() && timezone.parse::<chrono_tz::Tz>().is_err() =>
            {
                Err(garde::Error::new(TIMEZONE_INVALID))
            }
            _ => Ok(()),
        }
    }

    /// The expression must parse in the record's timezone.
    pub(super) fn cron_schedule(
        timezone: Option<&str>,
    ) -> impl FnOnce(&Option<String>, &()) -> garde::Result + '_ {
        move |value, _| {
            if value
                .as_deref()
                .is_some_and(|cron| !cron_valid(cron, timezone.unwrap_or_default()))
            {
                return Err(garde::Error::new(SCHEDULE_INVALID));
            }
            Ok(())
        }
    }
}

/// The task validations: [`Candidate`]'s field rules, then the two that need the
/// database: a name another task of the same owner already holds, and a
/// destination the owner cannot deliver to.
async fn validate(
    tx: &mut Transaction<'static, Postgres>,
    owner: TaskOwner,
    candidate: &Candidate,
    exclude_id: Option<i64>,
) -> Result<FieldErrors> {
    let mut validation = Validation::new();
    validation.check(candidate);
    // The name is unique per owner, by author or by principal.
    if let Some(name) = candidate.name.as_deref().filter(|name| !name.is_empty()) {
        let id = exclude_id.unwrap_or_default();
        let taken: bool = match owner {
            TaskOwner::Author(author_id) => {
                sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM scheduled_tasks \
                 WHERE author_id = $1 AND name = $2 AND id <> $3)",
                )
                .bind(author_id)
                .bind(name)
                .bind(id)
                .fetch_one(&mut **tx)
                .await?
            }
            TaskOwner::Channel(principal_id) => {
                sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM scheduled_tasks \
                 WHERE principal_id = $1 AND name = $2 AND id <> $3)",
                )
                .bind(principal_id)
                .bind(name)
                .bind(id)
                .fetch_one(&mut **tx)
                .await?
            }
        };
        if taken {
            validation.add("name", NAME_TAKEN);
        }
    }
    // `validate :delivery_destination_is_available`: only a destination that is
    // present and well-formed can be looked up.
    if !candidate.delivery_channel.is_empty()
        && delivery_destination_valid(&candidate.delivery_channel)
        && !delivery_destination_available(&mut *tx, owner, &candidate.delivery_channel).await?
    {
        validation.add("delivery_channel", DELIVERY_UNAVAILABLE);
    }
    let errors = validation.errors().clone();
    Ok(errors)
}

/// Console's database connections are read-only by default, so every write
/// runs in an explicitly read-write transaction.
pub(crate) async fn begin_write(pool: &PgPool) -> Result<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION READ WRITE")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

// ---------------------------------------------------------------------------
// Workflow dashboard reads
// ---------------------------------------------------------------------------
//
// The run rows come from the read-only view api-rs publishes
// (`CentaurWorkflowRun`), while schedules and run detail stay with the workflow
// runtime.

/// The view api-rs publishes for Console's workflow pages.
pub const WORKFLOW_RUNS_VIEW: &str = "centaur_readonly_workflow_runs";
/// The page size of the workflow dashboard.
pub const WORKFLOW_PAGE_SIZE: i64 = 50;

const RECENCY_SQL: &str = "coalesce(completed_at, failed_at, cancelled_at, started_at, \
     first_started_at, available_at, created_at)";
const WORKFLOW_KEY_SQL: &str = "coalesce(nullif(workflow_name, ''), nullif(task_name, ''))";
const DISPLAY_STATUS_SQL: &str = "CASE \
     WHEN cancelled_at IS NOT NULL THEN 'cancelled' \
     WHEN failed_at IS NOT NULL THEN 'failed' \
     WHEN completed_at IS NOT NULL THEN 'completed' \
     WHEN claimed OR state = 'running' THEN 'running' \
     ELSE coalesce(nullif(state, ''), 'unknown') END";

/// Timestamps as the ISO 8601 the console renders.
const RUN_COLUMNS: &str = "queue_name, run_id, task_id, task_name, workflow_name, harness_type, \
     state, attempts, max_attempts, claimed, \
     to_char(created_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as created_at, \
     to_char(first_started_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as first_started_at, \
     to_char(started_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as started_at, \
     to_char(completed_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as completed_at, \
     to_char(failed_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as failed_at, \
     to_char(cancelled_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as cancelled_at, \
     to_char(available_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') as available_at";

/// One row of [`WORKFLOW_RUNS_VIEW`].
#[derive(Debug, Clone, Default, sqlx::FromRow)]
pub struct WorkflowRunRow {
    pub queue_name: Option<String>,
    pub run_id: Option<String>,
    pub task_id: Option<String>,
    pub task_name: Option<String>,
    pub workflow_name: Option<String>,
    pub harness_type: Option<String>,
    pub state: Option<String>,
    pub attempts: Option<i32>,
    pub max_attempts: Option<i32>,
    #[sqlx(default)]
    pub claimed: Option<bool>,
    pub created_at: Option<String>,
    pub first_started_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub failed_at: Option<String>,
    pub cancelled_at: Option<String>,
    pub available_at: Option<String>,
    /// `COUNT(*) OVER (PARTITION BY workflow_key, queue_name)` where selected.
    #[sqlx(default)]
    pub queue_run_count: Option<i64>,
}

impl WorkflowRunRow {
    /// The display status of a run.
    pub fn display_status(&self) -> String {
        if self.cancelled_at.is_some() {
            return "cancelled".to_owned();
        }
        if self.failed_at.is_some() {
            return "failed".to_owned();
        }
        if self.completed_at.is_some() {
            return "completed".to_owned();
        }
        if self.claimed == Some(true) || self.state.as_deref() == Some("running") {
            return "running".to_owned();
        }
        match self.state.as_deref().map(str::trim) {
            Some(state) if !state.is_empty() => state.to_owned(),
            _ => "unknown".to_owned(),
        }
    }

    /// The workflow key of a run.
    pub fn workflow_key(&self) -> Option<String> {
        self.workflow_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or(self.task_name.as_deref().filter(|name| !name.is_empty()))
            .map(str::to_owned)
    }

    /// The workflow name label of a run.
    pub fn workflow_name_label(&self) -> String {
        self.workflow_key()
            .unwrap_or_else(|| "unknown workflow".to_owned())
    }

    /// The queue label of a run.
    pub fn queue_label(&self) -> String {
        queue_label(self.queue_name.as_deref())
    }

    /// When a run started, or when it was created.
    pub fn started_or_created_at(&self) -> Option<&str> {
        self.started_at
            .as_deref()
            .or(self.first_started_at.as_deref())
            .or(self.created_at.as_deref())
    }

    /// When a run reached a terminal state.
    pub fn terminal_at(&self) -> Option<&str> {
        self.completed_at
            .as_deref()
            .or(self.failed_at.as_deref())
            .or(self.cancelled_at.as_deref())
    }

    /// The run's recency, as the sortable ISO 8601 string.
    pub fn recency_at(&self) -> Option<&str> {
        self.terminal_at()
            .or(self.started_at.as_deref())
            .or(self.first_started_at.as_deref())
            .or(self.available_at.as_deref())
            .or(self.created_at.as_deref())
    }

    /// `WorkflowDashboard.engine_label`.
    pub fn engine_label(&self) -> Option<String> {
        engine_label(self.harness_type.as_deref())
    }
}

/// `WorkflowDashboard.queue_label_for`.
pub fn queue_label(queue_name: Option<&str>) -> String {
    let suffix = queue_name
        .unwrap_or_default()
        .trim_start_matches("centaur_workflows")
        .trim_start_matches('_');
    if suffix.is_empty() {
        "default".to_owned()
    } else {
        suffix.replace('_', " ")
    }
}

/// `WorkflowDashboard.engine_label`.
pub fn engine_label(harness_type: Option<&str>) -> Option<String> {
    let value = harness_type.unwrap_or_default();
    if value.is_empty() {
        return None;
    }
    let label = match value {
        "codex" => "Codex",
        "claudecode" => "Claude Code",
        "amp" => "Amp",
        "pi" => "Pi",
        "omp" => "OMP",
        other => {
            let cleaned = other.replace(['_', '-'], " ");
            let words: Vec<String> = cleaned
                .split_whitespace()
                .map(|word| {
                    let mut chars = word.chars();
                    match chars.next() {
                        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                        None => String::new(),
                    }
                })
                .filter(|word| !word.is_empty())
                .collect();
            return Some(words.join(" "));
        }
    };
    Some(label.to_owned())
}

/// `WorkflowDashboard.source_url`.
pub fn source_url(source_path: Option<&str>) -> Option<String> {
    let path = source_path.unwrap_or_default();
    if path.is_empty() {
        return None;
    }
    Some(match path.strip_prefix("centaur-tempo/") {
        Some(rest) => format!("https://github.com/tempoxyz/centaur-tempo/blob/main/{rest}"),
        None => format!("https://github.com/paradigmxyz/centaur/blob/main/{path}"),
    })
}

/// `WorkflowDashboard.schedule_label`, from a registered schedule.
pub fn workflow_schedule_label(schedule: &Value) -> Option<String> {
    let kind = schedule.get("kind")?.as_object()?;
    match kind.get("type").and_then(Value::as_str)? {
        "cron" => Some(format!("cron {}", kind.get("cron")?.as_str()?)),
        "interval" => {
            let seconds = kind
                .get("interval_seconds")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            if seconds >= 60 && seconds % 60 == 0 {
                Some(format!("every {}m", seconds / 60))
            } else {
                Some(format!("every {seconds}s"))
            }
        }
        _ => None,
    }
}

/// Whether Console's run view exists.
pub async fn workflow_database_available(pool: &PgPool) -> Result<bool> {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(WORKFLOW_RUNS_VIEW)
        .fetch_one(pool)
        .await?;
    Ok(exists)
}

/// `CentaurWorkflowRun.workflow_count`.
pub async fn workflow_count(pool: &PgPool) -> Result<i64> {
    sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM (SELECT DISTINCT {WORKFLOW_KEY_SQL} FROM {WORKFLOW_RUNS_VIEW}) keys"
    ))
    .fetch_one(pool)
    .await
    .map_err(Into::into)
}

/// `CentaurWorkflowRun.latest_per_workflow`.
pub async fn latest_per_workflow(
    pool: &PgPool,
    limit: i64,
    offset: i64,
) -> Result<Vec<WorkflowRunRow>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {RUN_COLUMNS} FROM (
             SELECT DISTINCT ON ({WORKFLOW_KEY_SQL}) *
             FROM {WORKFLOW_RUNS_VIEW}
             ORDER BY {WORKFLOW_KEY_SQL}, {RECENCY_SQL} DESC, task_id DESC
         ) latest
         ORDER BY {RECENCY_SQL} DESC, task_id DESC
         LIMIT $1 OFFSET $2"
    ))
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?)
}

/// `CentaurWorkflowRun.latest_per_queue`: the newest run of every queue for
/// each workflow key, each carrying that queue's total run count.
pub async fn latest_per_queue(
    pool: &PgPool,
    keys: &[String],
) -> Result<HashMap<String, Vec<WorkflowRunRow>>> {
    let mut groups: HashMap<String, Vec<WorkflowRunRow>> = HashMap::new();
    if keys.is_empty() {
        return Ok(groups);
    }
    let rows: Vec<WorkflowRunRow> = sqlx::query_as(&format!(
        "SELECT DISTINCT ON ({WORKFLOW_KEY_SQL}, queue_name) {RUN_COLUMNS}, \
             COUNT(*) OVER (PARTITION BY {WORKFLOW_KEY_SQL}, queue_name) AS queue_run_count
         FROM {WORKFLOW_RUNS_VIEW}
         WHERE {WORKFLOW_KEY_SQL} = ANY($1)
         ORDER BY {WORKFLOW_KEY_SQL}, queue_name, {RECENCY_SQL} DESC, task_id DESC"
    ))
    .bind(keys)
    .fetch_all(pool)
    .await?;
    for run in rows {
        let key = run.workflow_key().unwrap_or_default();
        groups.entry(key).or_default().push(run);
    }
    for runs in groups.values_mut() {
        // Newest queue activity first.
        runs.sort_by(|left, right| right.recency_at().cmp(&left.recency_at()));
    }
    Ok(groups)
}

/// `CentaurWorkflowRun.for_workflow`.
pub async fn workflow_runs(
    pool: &PgPool,
    workflow_name: &str,
    limit: i64,
    offset: i64,
    status: Option<&str>,
    queue: Option<&str>,
) -> Result<Vec<WorkflowRunRow>> {
    let mut sql = format!(
        "SELECT {RUN_COLUMNS} FROM {WORKFLOW_RUNS_VIEW} WHERE {}",
        workflow_scope(status, queue)
    );
    // `workflow_scope` uses $1 for the workflow name and then one bind per
    // present filter, so the page binds follow them instead of reusing $2/$3.
    let limit_param = 2 + i32::from(status.is_some()) + i32::from(queue.is_some());
    sql.push_str(&format!(
        " ORDER BY {RECENCY_SQL} DESC, task_id DESC LIMIT ${limit_param} OFFSET ${}",
        limit_param + 1
    ));
    let mut query = sqlx::query_as::<_, WorkflowRunRow>(&sql).bind(workflow_name);
    if let Some(status) = status {
        query = query.bind(status);
    }
    if let Some(queue) = queue {
        query = query.bind(queue);
    }
    let rows: Vec<WorkflowRunRow> = query.bind(limit).bind(offset).fetch_all(pool).await?;
    Ok(rows)
}

/// `CentaurWorkflowRun.run_count`.
pub async fn workflow_run_count(
    pool: &PgPool,
    workflow_name: &str,
    status: Option<&str>,
    queue: Option<&str>,
) -> Result<i64> {
    let sql = format!(
        "SELECT COUNT(*) FROM {WORKFLOW_RUNS_VIEW} WHERE {}",
        workflow_scope(status, queue)
    );
    let mut query = sqlx::query_scalar(&sql).bind(workflow_name);
    if let Some(status) = status {
        query = query.bind(status);
    }
    if let Some(queue) = queue {
        query = query.bind(queue);
    }
    query.fetch_one(pool).await.map_err(Into::into)
}

/// `CentaurWorkflowRun.status_counts`.
pub async fn workflow_status_counts(
    pool: &PgPool,
    workflow_name: &str,
) -> Result<BTreeMap<String, i64>> {
    let rows: Vec<(Option<String>, i64)> = sqlx::query_as(&format!(
        "SELECT {DISPLAY_STATUS_SQL} AS status, COUNT(*) AS count \
         FROM {WORKFLOW_RUNS_VIEW} WHERE workflow_name = $1 \
             OR ((workflow_name IS NULL OR workflow_name = '') AND task_name = $1) \
         GROUP BY {DISPLAY_STATUS_SQL}"
    ))
    .bind(workflow_name)
    .fetch_all(pool)
    .await?;
    let mut counts = BTreeMap::new();
    for (status, count) in rows {
        counts.insert(status.unwrap_or_default(), count);
    }
    Ok(counts)
}

/// `CentaurWorkflowRun.queue_names`.
pub async fn workflow_queue_names(pool: &PgPool, workflow_name: &str) -> Result<Vec<String>> {
    let rows: Vec<Option<String>> = sqlx::query_scalar(&format!(
        "SELECT DISTINCT queue_name FROM {WORKFLOW_RUNS_VIEW} WHERE workflow_name = $1 \
             OR ((workflow_name IS NULL OR workflow_name = '') AND task_name = $1) \
         ORDER BY queue_name"
    ))
    .bind(workflow_name)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().flatten().collect())
}

/// `CentaurWorkflowRun.workflow_scope`, with the binds `$2`/`$3` when present.
fn workflow_scope(status: Option<&str>, queue: Option<&str>) -> String {
    let mut scope =
        "(workflow_name = $1 OR ((workflow_name IS NULL OR workflow_name = '') AND task_name = $1))"
            .to_owned();
    let mut next = 2;
    if status.is_some() {
        scope.push_str(&format!(" AND {DISPLAY_STATUS_SQL} = ${next}"));
        next += 1;
    }
    if queue.is_some() {
        scope.push_str(&format!(" AND queue_name = ${next}"));
    }
    scope
}

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------
//
// Skills, skill editors, authoring, and the catalog scopes.

/// The largest skill document, in bytes.
pub const SKILL_MAX_DOCUMENT_BYTES: usize = 64 * 1024;
/// Names a skill may not take.
pub const SKILL_RESERVED_NAMES: [&str; 1] = ["search"];
/// The message for a skill write based on stale content.
pub const SKILL_STALE_MESSAGE: &str =
    "This skill changed since you opened it. Review the latest content before saving.";
/// The error for an editor selection naming an unavailable user.
pub const SKILL_EDITORS_UNAVAILABLE: &str = "include an unavailable user";

/// A skill row with its author, as the catalog returns it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Skill {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub description: String,
    pub content: String,
    pub visibility: String,
    pub lock_version: i32,
    pub created_at: String,
    pub updated_at: String,
    pub author_name: Option<String>,
    pub author_email: String,
}

impl Skill {
    pub fn oid(&self) -> String {
        model::Skill::oid(self.id)
    }

    /// `summary_payload`'s author: the name, else the email.
    pub fn author_label(&self) -> String {
        self.author_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.author_email)
            .to_owned()
    }

    pub fn shared(&self) -> bool {
        self.visibility == "shared"
    }
}

/// A user the console lists as an editor or a candidate.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SkillUser {
    pub id: i64,
    pub email: String,
    pub name: Option<String>,
    pub status: String,
}

impl SkillUser {
    pub fn oid(&self) -> String {
        model::User::oid(self.id)
    }
}

/// Which catalog tab a listing reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillsTab {
    /// The directory tab: every shared active skill.
    Directory,
    /// The caller's own and edited skills.
    Mine,
    /// Shared skills plus the caller's own.
    Catalog,
}

/// Any term matches, not every term.
const SKILL_SEARCH_QUERY: &str =
    "replace(plainto_tsquery('english', ?)::text, ' & ', ' | ')::tsquery";

const SKILL_COLUMNS: &str = "skills.id, skills.user_id, skills.name, skills.description, \
     skills.content, skills.visibility, skills.lock_version, users.name AS author_name, \
     users.email AS author_email, \
     to_char(skills.created_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as created_at, \
     to_char(skills.updated_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as updated_at";

/// `Skill.editable_by(user)`: the caller's own active skills and the ones it
/// edits.
fn editable_scope(bind: &str) -> String {
    format!(
        "(skills.archived_at IS NULL AND (skills.user_id = {bind} \
          OR skills.id IN (SELECT skill_id FROM skill_editors WHERE user_id = {bind})))"
    )
}

/// `Skill.catalog_visible_to(user)`: shared active skills plus the caller's.
fn catalog_scope(bind: &str) -> String {
    format!(
        "(skills.archived_at IS NULL AND (skills.visibility = 'shared' OR {editable}))",
        editable = editable_scope(bind)
    )
}

/// [`catalog_scope`] for a caller that may be anonymous.
fn catalog_scope_for(viewer_id: Option<i64>) -> String {
    match viewer_id {
        Some(viewer_id) => catalog_scope(&format!("{viewer_id}")),
        None => "(skills.archived_at IS NULL AND skills.visibility = 'shared')".to_owned(),
    }
}

/// The skill catalog listing: [`SkillsTab`] plus the full-text search.
pub async fn list_skills(
    pool: &PgPool,
    viewer_id: Option<i64>,
    tab: SkillsTab,
    query: Option<&str>,
) -> Result<Vec<Skill>> {
    let scope = match (tab, viewer_id) {
        (SkillsTab::Mine, Some(viewer_id)) => editable_scope(&format!("{viewer_id}")),
        (SkillsTab::Mine, None) => "false".to_owned(),
        // The directory tab is the shared active scope, not the wider catalog
        // scope the show action uses.
        (SkillsTab::Directory, _) => {
            "(skills.archived_at IS NULL AND skills.visibility = 'shared')".to_owned()
        }
        (SkillsTab::Catalog, viewer_id) => catalog_scope_for(viewer_id),
    };
    match query.map(str::trim).filter(|query| !query.is_empty()) {
        Some(query) => {
            let search = SKILL_SEARCH_QUERY.replace('?', "$1");
            Ok(sqlx::query_as(&format!(
                "SELECT {SKILL_COLUMNS} FROM skills JOIN users ON users.id = skills.user_id \
                 WHERE {scope} AND skills.search_vector @@ {search} \
                 ORDER BY lower(skills.name) = lower($1) DESC, \
                     ts_rank(skills.search_vector, {search}) DESC, \
                     skills.updated_at DESC, skills.id ASC"
            ))
            .bind(query)
            .fetch_all(pool)
            .await?)
        }
        None => Ok(sqlx::query_as(&format!(
            "SELECT {SKILL_COLUMNS} FROM skills JOIN users ON users.id = skills.user_id \
                 WHERE {scope} ORDER BY skills.updated_at DESC, skills.id ASC"
        ))
        .fetch_all(pool)
        .await?),
    }
}

/// The skill a caller may read: by OID, or by name for the sandbox API.
pub async fn catalog_skill(
    pool: &PgPool,
    viewer_id: Option<i64>,
    reference: &str,
) -> Result<Option<Skill>> {
    let Some(id) = model::Skill::decode_oid(reference) else {
        return Ok(None);
    };
    find_skill(pool, &catalog_scope_for(viewer_id), id).await
}

/// `warden` for the sandbox API: a name lookup within the catalog.
pub async fn catalog_skill_by_name(
    pool: &PgPool,
    viewer_id: Option<i64>,
    name: &str,
) -> Result<Option<Skill>> {
    let rows: Vec<Skill> = sqlx::query_as(&format!(
        "SELECT {SKILL_COLUMNS} FROM skills JOIN users ON users.id = skills.user_id \
         WHERE {} AND skills.name = $1",
        catalog_scope_for(viewer_id)
    ))
    .bind(name)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().next())
}

/// A skill the caller may edit.
pub async fn editable_skill(pool: &PgPool, user_id: i64, reference: &str) -> Result<Option<Skill>> {
    let Some(id) = model::Skill::decode_oid(reference) else {
        return Ok(None);
    };
    find_skill(pool, &editable_scope(&format!("{user_id}")), id).await
}

/// The unshare admin scope: any shared active skill, plus the admin's own.
pub async fn admin_unshareable_skill(
    pool: &PgPool,
    user_id: i64,
    reference: &str,
) -> Result<Option<Skill>> {
    let Some(id) = model::Skill::decode_oid(reference) else {
        return Ok(None);
    };
    find_skill(
        pool,
        &format!(
            "(skills.archived_at IS NULL AND (skills.visibility = 'shared' OR skills.user_id = {user_id}))"
        ),
        id,
    )
    .await
}

/// A skill the caller owns and has not archived.
pub async fn owned_skill(pool: &PgPool, user_id: i64, reference: &str) -> Result<Option<Skill>> {
    let Some(id) = model::Skill::decode_oid(reference) else {
        return Ok(None);
    };
    find_skill(
        pool,
        &format!("(skills.archived_at IS NULL AND skills.user_id = {user_id})"),
        id,
    )
    .await
}

async fn find_skill(pool: &PgPool, scope: &str, id: i64) -> Result<Option<Skill>> {
    let rows: Vec<Skill> = sqlx::query_as(&format!(
        "SELECT {SKILL_COLUMNS} FROM skills JOIN users ON users.id = skills.user_id \
         WHERE {scope} AND skills.id = $1"
    ))
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().next())
}

/// The skill behind `id`, whatever its state.
pub async fn skill_by_id(pool: &PgPool, id: i64) -> Result<Option<Skill>> {
    find_skill(pool, "true", id).await
}

/// `skill.editors.active.order(:email)`.
pub async fn skill_editors(pool: &PgPool, skill_id: i64) -> Result<Vec<SkillUser>> {
    Ok(sqlx::query_as(
        "SELECT users.id, users.email, users.name, users.status \
         FROM skill_editors JOIN users ON users.id = skill_editors.user_id \
         WHERE skill_editors.skill_id = $1 AND users.status = 'active' \
         ORDER BY users.email ASC",
    )
    .bind(skill_id)
    .fetch_all(pool)
    .await?)
}

/// Whether `user` is an editor of the skill.
pub async fn skill_edited_by(pool: &PgPool, skill_id: i64, user_id: i64) -> Result<bool> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM skill_editors WHERE skill_id = $1 AND user_id = $2)",
    )
    .bind(skill_id)
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

/// Active users other than `user_id`, ordered by email, optionally filtered by
/// a lowercase substring of the email or name.
pub async fn skill_editor_candidates(
    pool: &PgPool,
    user_id: i64,
    query: Option<&str>,
) -> Result<Vec<SkillUser>> {
    Ok(sqlx::query_as(
        "SELECT id, email, name, status FROM users \
         WHERE status = 'active' AND id <> $1 AND email <> $3 \
           AND ($2::text IS NULL OR lower(email) LIKE $2 \
                OR lower(coalesce(name, '')) LIKE $2) \
         ORDER BY email ASC",
    )
    .bind(user_id)
    .bind(query.map(|query| format!("%{}%", escape_like(&query.to_lowercase()))))
    .bind(crate::SYSTEM_USER_EMAIL)
    .fetch_all(pool)
    .await?)
}

/// Escape `\`, `%`, and `_` for an ILIKE pattern.
fn escape_like(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// The SKILL.md Console shows and checksums.
pub fn skill_document(name: &str, description: &str, content: &str) -> String {
    format!(
        "---\nname: {}\ndescription: {}\n---\n\n{content}\n",
        yaml_scalar(name),
        yaml_scalar(description)
    )
}

/// The checksum of the skill document.
pub fn skill_checksum(name: &str, description: &str, content: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(skill_document(name, description, content).as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A YAML scalar as Psych dumps it: plain when unambiguous, quoted otherwise.
fn yaml_scalar(value: &str) -> String {
    let plain = !value.is_empty()
        && !value.trim().is_empty()
        && value.trim() == value
        && value
            .chars()
            .all(|character| character.is_alphanumeric() || " _.,'()-/@:+".contains(character))
        && !value.contains(": ")
        && !value.contains(" #")
        && !value.ends_with(':')
        && !matches!(
            value.to_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
        )
        && value.parse::<f64>().is_err();
    if plain {
        return value.to_owned();
    }
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

/// A skill write's attributes, already permitted.
#[derive(Debug, Clone, Default)]
pub struct SkillDraft {
    pub name: Option<String>,
    pub description: Option<String>,
    pub content: Option<String>,
    pub visibility: Option<String>,
    pub lock_version: Option<i32>,
    /// Whether the body named `lock_version` (an explicit stale check).
    pub lock_version_supplied: bool,
}

/// A failed skill write, in the shape a route renders.
#[derive(Debug)]
pub enum SkillWriteError {
    /// Validation messages with content reported as `instructions`.
    Invalid(FieldErrors),
    /// The write's `lock_version` is stale.
    Stale,
    /// The skill does not exist.
    Missing,
    /// The name is already used.
    Conflicts,
    Error(super::super::Error),
}

impl From<sqlx::Error> for SkillWriteError {
    fn from(error: sqlx::Error) -> Self {
        if is_unique_violation(&error) {
            return Self::Conflicts;
        }
        Self::Error(error.into())
    }
}

impl From<crate::Error> for SkillWriteError {
    fn from(error: crate::Error) -> Self {
        Self::Error(error)
    }
}

impl From<active_record_encryption::Error> for SkillWriteError {
    fn from(error: active_record_encryption::Error) -> Self {
        Self::Error(error.into())
    }
}

pub type SkillWriteResult<T> = std::result::Result<T, SkillWriteError>;

/// The skill validations, with `content` errors reported under `content` (the
/// routes rename them at the boundary).
///
/// [`SkillAttributes`] is the merged record a write validates: the stored values
/// for fields the write leaves alone, and the submitted ones otherwise. Every
/// rule but the uniqueness lookup is a garde attribute; the name another active
/// skill already holds needs the database.
pub async fn validate_skill(
    executor: &mut PgConnection,
    exclude_id: Option<i64>,
    attributes: SkillAttributes<'_>,
) -> Result<FieldErrors> {
    let mut validation = Validation::new();
    validation.check(&attributes);
    // A name no active skill already holds.
    if !attributes.name.is_empty() {
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM skills WHERE archived_at IS NULL AND name = $1 \
                AND ($2::bigint IS NULL OR id <> $2))",
        )
        .bind(attributes.name)
        .bind(exclude_id)
        .fetch_one(&mut *executor)
        .await?;
        if taken {
            validation.add("name", "is already used by another skill");
        }
    }
    let errors = validation.errors().clone();
    Ok(errors)
}

/// The values a skill write validates: the stored or submitted attributes, and
/// the visibility a write asked for.
#[derive(Debug, garde::Validate)]
pub struct SkillAttributes<'a> {
    // A present, non-empty document within the size limit.
    #[garde(
        length(chars, min = 1),
        custom(skill_rules::document_size(self.name, self.description))
    )]
    pub content: &'a str,
    // A present name of at most 64 characters: lower-case words joined by
    // dashes, and not a reserved name.
    #[garde(
        length(chars, min = 1, max = 64),
        pattern(r"^[a-z0-9]+(?:-[a-z0-9]+)*$"),
        custom(skill_rules::name_not_reserved)
    )]
    pub name: &'a str,
    // A present description.
    #[garde(length(chars, min = 1))]
    pub description: &'a str,
    // The visibility, which only the sandbox's create action writes.
    #[garde(custom(skill_rules::visibility_included))]
    pub visibility: Option<&'a str>,
}

/// The field rules garde has no built-in for.
mod skill_rules {
    use super::{SKILL_MAX_DOCUMENT_BYTES, SKILL_RESERVED_NAMES, skill_document, skill_visibility};
    use crate::validation::rules::INCLUSION_MESSAGE;

    /// The name is not reserved.
    pub(super) fn name_not_reserved(value: &str, _: &()) -> garde::Result {
        if SKILL_RESERVED_NAMES.contains(&value) {
            return Err(garde::Error::new("is reserved"));
        }
        Ok(())
    }

    /// The visibility is `private` or `shared` only.
    pub(super) fn visibility_included(value: &Option<&str>, _: &()) -> garde::Result {
        match value {
            Some(value) if skill_visibility(value).is_none() => {
                Err(garde::Error::new(INCLUSION_MESSAGE))
            }
            _ => Ok(()),
        }
    }

    /// `validate :document_size`: the rendered SKILL.md is at most 64 KiB.
    pub(super) fn document_size<'a>(
        name: &'a str,
        description: &'a str,
    ) -> impl FnOnce(&str, &()) -> garde::Result + 'a {
        move |content, _| {
            if skill_document(name, description, content).len() > SKILL_MAX_DOCUMENT_BYTES {
                return Err(garde::Error::new(
                    "makes the complete SKILL.md exceed 64 KiB",
                ));
            }
            Ok(())
        }
    }
}

/// `Skill`'s `enum :visibility`: the values a write may ask for.
pub fn skill_visibility(value: &str) -> Option<&str> {
    matches!(value, "private" | "shared").then_some(value)
}

/// The active users named by `oids`, and
/// whether the selection was fully resolvable.
async fn resolve_editors(
    executor: &mut PgConnection,
    owner_id: i64,
    oids: &[String],
) -> Result<(Vec<SkillUser>, bool)> {
    let mut submitted: Vec<String> = Vec::new();
    for reference in oids {
        let reference = reference.trim();
        if reference.is_empty() || submitted.iter().any(|seen| seen == reference) {
            continue;
        }
        submitted.push(reference.to_owned());
    }
    let ids: Vec<i64> = submitted
        .iter()
        .filter_map(|reference| model::User::decode_oid(reference))
        .collect();
    let rows: Vec<SkillUser> = sqlx::query_as(
        "SELECT id, email, name, status FROM users \
         WHERE id = ANY($1) AND status = 'active' AND email <> $2",
    )
    .bind(&ids)
    .bind(crate::SYSTEM_USER_EMAIL)
    .fetch_all(&mut *executor)
    .await?;
    let mut users: Vec<SkillUser> = Vec::new();
    for user in rows {
        if user.id == owner_id {
            continue;
        }
        users.push(user);
    }
    let by_id: HashMap<i64, &SkillUser> = users.iter().map(|user| (user.id, user)).collect();
    let ordered: Vec<SkillUser> = ids
        .iter()
        .filter_map(|id| by_id.get(id).map(|user| (*user).clone()))
        .collect();
    let invalid = ids.len() != submitted.len() || ordered.len() != ids.len();
    Ok((ordered, invalid))
}

/// Postgres' `unique_violation`.
fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(database) if database.code().as_deref() == Some("23505")
    )
}

/// Save a new skill.
pub async fn create_skill(
    pool: &PgPool,
    owner_id: i64,
    draft: SkillDraft,
    editor_oids: &[String],
) -> SkillWriteResult<Skill> {
    let mut tx = begin_write(pool).await?;
    let (editors, invalid) = resolve_editors(&mut tx, owner_id, editor_oids).await?;
    let (name, description, content) = merged(None, &draft);
    validated(
        &mut tx,
        None,
        &draft,
        (&name, &description, &content),
        invalid,
    )
    .await?;
    let visibility = draft
        .visibility
        .as_deref()
        .and_then(skill_visibility)
        .unwrap_or("shared")
        .to_owned();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO skills (user_id, name, description, content, visibility, shared_at, \
             lock_version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, \
             CASE WHEN $5 = 'shared' THEN now() ELSE NULL END, 0, now(), now()) \
         RETURNING id",
    )
    .bind(owner_id)
    .bind(&name)
    .bind(&description)
    .bind(&content)
    .bind(&visibility)
    .fetch_one(&mut *tx)
    .await?;
    let sql = format!(
        "{} ON CONFLICT DO NOTHING",
        write::insert_sql("skill_editors", &["skill_id", "user_id"])
    );
    for editor in &editors {
        sqlx::query(&sql)
            .bind(id)
            .bind(editor.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    skill_by_id(pool, id).await?.ok_or(SkillWriteError::Missing)
}

fn errors_with_editors(mut errors: FieldErrors, invalid: bool) -> FieldErrors {
    if invalid {
        errors.add("editors", SKILL_EDITORS_UNAVAILABLE);
    }
    errors
}

/// The attributes a skill write stores: the draft's values, or the stored
/// skill's where the draft leaves one out. A create has no stored skill, so an
/// omitted attribute is blank.
fn merged(skill: Option<&Skill>, draft: &SkillDraft) -> (String, String, String) {
    let stored = |value: &Option<String>, of: fn(&Skill) -> &String| {
        value
            .clone()
            .or_else(|| skill.map(of).cloned())
            .unwrap_or_default()
    };
    (
        stored(&draft.name, |skill| &skill.name),
        stored(&draft.description, |skill| &skill.description),
        stored(&draft.content, |skill| &skill.content),
    )
}

/// The shared skill-write guard: validate the merged attributes and refuse the
/// write when either the validation or the editor selection failed.
async fn validated(
    executor: &mut PgConnection,
    skill_id: Option<i64>,
    draft: &SkillDraft,
    (name, description, content): (&str, &str, &str),
    invalid: bool,
) -> SkillWriteResult<()> {
    let errors = validate_skill(
        executor,
        skill_id,
        SkillAttributes {
            name,
            description,
            content,
            visibility: draft.visibility.as_deref(),
        },
    )
    .await?;
    if !errors.is_empty() || invalid {
        return Err(SkillWriteError::Invalid(errors_with_editors(
            errors, invalid,
        )));
    }
    Ok(())
}

/// Updates an existing skill. `editor_oids` of `None` keeps the current editors.
pub async fn update_skill(
    pool: &PgPool,
    skill: &Skill,
    draft: SkillDraft,
    editor_oids: Option<&[String]>,
) -> SkillWriteResult<Skill> {
    let mut tx = begin_write(pool).await?;
    let (editors, invalid) = match editor_oids {
        Some(oids) => resolve_editors(&mut tx, skill.user_id, oids).await?,
        None => (Vec::new(), false),
    };
    let (name, description, content) = merged(Some(skill), &draft);
    validated(
        &mut tx,
        Some(skill.id),
        &draft,
        (&name, &description, &content),
        invalid,
    )
    .await?;
    let expected = draft.lock_version.unwrap_or(skill.lock_version);
    let visibility = draft
        .visibility
        .as_deref()
        .and_then(skill_visibility)
        .unwrap_or(skill.visibility.as_str());
    let updated = sqlx::query(
        "UPDATE skills SET name = $2, description = $3, content = $4, visibility = $5, \
             shared_at = CASE WHEN $5 = 'shared' THEN coalesce(shared_at, now()) ELSE NULL END, \
             lock_version = lock_version + 1, updated_at = now() \
         WHERE id = $1 AND lock_version = $6",
    )
    .bind(skill.id)
    .bind(&name)
    .bind(&description)
    .bind(&content)
    .bind(visibility)
    .bind(expected)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(SkillWriteError::Stale);
    }
    if editor_oids.is_some() {
        replace_skill_editors(&mut tx, skill.id, &editors).await?;
    }
    tx.commit().await?;
    skill_by_id(pool, skill.id)
        .await?
        .ok_or(SkillWriteError::Missing)
}

async fn replace_skill_editors(
    executor: &mut PgConnection,
    skill_id: i64,
    editors: &[SkillUser],
) -> Result<()> {
    let ids: Vec<i64> = editors.iter().map(|editor| editor.id).collect();
    sqlx::query("DELETE FROM skill_editors WHERE skill_id = $1 AND NOT (user_id = ANY($2))")
        .bind(skill_id)
        .bind(&ids)
        .execute(&mut *executor)
        .await?;
    for editor in editors {
        sqlx::query(
            "INSERT INTO skill_editors (skill_id, user_id, created_at, updated_at) \
             VALUES ($1, $2, now(), now()) ON CONFLICT DO NOTHING",
        )
        .bind(skill_id)
        .bind(editor.id)
        .execute(&mut *executor)
        .await?;
    }
    Ok(())
}

/// Share the skill, preserving `shared_at`.
pub async fn share_skill(pool: &PgPool, skill_id: i64) -> Result<()> {
    set_public(pool, skill_id, true).await
}

/// Make the skill private again.
pub async fn unshare_skill(pool: &PgPool, skill_id: i64) -> Result<()> {
    set_public(pool, skill_id, false).await
}

/// The one visibility write behind both; the optimistic lock is bumped only
/// when an attribute actually changes.
async fn set_public(pool: &PgPool, skill_id: i64, shared: bool) -> Result<()> {
    sqlx::query(
        "UPDATE skills SET visibility = $2, \
             shared_at = CASE WHEN $2 = 'shared' THEN coalesce(shared_at, now()) ELSE NULL END, \
             updated_at = now(), lock_version = lock_version + 1 \
         WHERE id = $1 AND (visibility <> $2 OR (shared_at IS NULL) = $3)",
    )
    .bind(skill_id)
    .bind(if shared { "shared" } else { "private" })
    .bind(shared)
    .execute(pool)
    .await?;
    Ok(())
}

/// Archive the skill: private, with `shared_at` cleared.
pub async fn archive_skill(pool: &PgPool, skill_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE skills SET archived_at = now(), visibility = 'private', shared_at = NULL, \
             updated_at = now(), lock_version = lock_version + 1 WHERE id = $1",
    )
    .bind(skill_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Assign an active user to the skill and touch it.
pub async fn add_skill_editor(pool: &PgPool, skill_id: i64, user_id: i64) -> Result<()> {
    let mut tx = begin_write(pool).await?;
    let inserted = sqlx::query(
        "INSERT INTO skill_editors (skill_id, user_id, created_at, updated_at) \
         VALUES ($1, $2, now(), now()) ON CONFLICT DO NOTHING",
    )
    .bind(skill_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() > 0 {
        touch_skill(&mut tx, skill_id).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Remove an editor from the skill, returning whether one was removed.
pub async fn remove_skill_editor(pool: &PgPool, skill_id: i64, user_id: i64) -> Result<bool> {
    let mut tx = begin_write(pool).await?;
    let deleted = sqlx::query("DELETE FROM skill_editors WHERE skill_id = $1 AND user_id = $2")
        .bind(skill_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    if deleted.rows_affected() == 0 {
        return Ok(false);
    }
    touch_skill(&mut tx, skill_id).await?;
    tx.commit().await?;
    Ok(true)
}

async fn touch_skill(executor: &mut PgConnection, skill_id: i64) -> Result<()> {
    // Refreshes `updated_at` and bumps `lock_version` like any other update.
    sqlx::query(
        "UPDATE skills SET updated_at = now(), lock_version = lock_version + 1 WHERE id = $1",
    )
    .bind(skill_id)
    .execute(&mut *executor)
    .await?;
    Ok(())
}

/// The user behind a sandbox editor reference: an OID or an email.
pub async fn resolve_editor_user(
    pool: &PgPool,
    reference: &str,
    active_only: bool,
) -> Result<Option<SkillUser>> {
    let user: Option<SkillUser> = if let Some(id) = model::User::decode_oid(reference) {
        model::find_by_id::<model::User, SkillUser, _>(pool, id).await?
    } else {
        sqlx::query_as("SELECT id, email, name, status FROM users WHERE email = $1")
            .bind(reference.to_lowercase())
            .fetch_optional(pool)
            .await?
    };
    let Some(user) = user.filter(|user| user.email != crate::SYSTEM_USER_EMAIL) else {
        return Ok(None);
    };
    if active_only && user.status != "active" {
        return Ok(None);
    }
    Ok(Some(user))
}

// ---------------------------------------------------------------------------
// Personal SSH key
// ---------------------------------------------------------------------------
//
// Personal SSH keys, their grants, and the `/api/v1/console/me/ssh_key`
// payload. The private key lives in a `secret_sources` row, encrypted with
// Console's Active Record encryption keys.

/// The name a personal SSH key gets when the write sends none.
pub const PERSONAL_SSH_KEY_DEFAULT_NAME: &str = "My SSH key";
/// The default priority of a direct grant.
const DEFAULT_DIRECT_PRIORITY: i32 = 100;
/// The message an unsupported public key renders.
pub const PUBLIC_KEY_FORMAT_MESSAGE: &str =
    "must be an Ed25519, RSA, or ECDSA public key in OpenSSH format";

/// The caller's personal SSH key, without its private material.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PersonalSshKey {
    pub id: i64,
    pub name: String,
    pub public_key: String,
    pub created_at: String,
    pub updated_at: String,
    /// Whether the key still has a stored source.
    pub has_private_key: bool,
}

impl PersonalSshKey {
    pub fn oid(&self) -> String {
        model::SshKeySecret::oid(self.id)
    }
}

/// One grant of the personal key.
#[derive(Debug, Clone)]
pub struct PersonalSshKeyGrant {
    pub id: i64,
    pub role_id: Option<i64>,
    pub principal_id: Option<i64>,
    pub grantee_oid: String,
    pub grantee_name: Option<String>,
    pub grantee_foreign_id: Option<String>,
    pub principal_kind: Option<String>,
}

impl PersonalSshKeyGrant {
    pub fn oid(&self) -> String {
        model::Grant::oid(self.id)
    }

    /// The first non-empty of `grantee_name`, `grantee_foreign_id`, and the
    /// grantee oid.
    pub fn label(&self) -> String {
        self.grantee_name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or(self
                .grantee_foreign_id
                .as_deref()
                .filter(|foreign_id| !foreign_id.is_empty()))
            .map(str::to_owned)
            .unwrap_or_else(|| self.grantee_oid.clone())
    }
}

const SSH_KEY_COLUMNS: &str = "k.id, k.name, k.public_key, \
     EXISTS (SELECT 1 FROM secret_sources s WHERE s.ssh_key_secret_id = k.id) AS has_private_key, \
     to_char(k.created_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as created_at, \
     to_char(k.updated_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') as updated_at";

/// The caller's personal SSH key.
pub async fn personal_ssh_key(pool: &PgPool, owner_id: i64) -> Result<Option<PersonalSshKey>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {SSH_KEY_COLUMNS} FROM ssh_key_secrets k WHERE k.owner_id = $1"
    ))
    .bind(owner_id)
    .fetch_optional(pool)
    .await?)
}

/// `key.grants.includes(:principal, :role).order(:id)`.
pub async fn personal_ssh_key_grants(
    pool: &PgPool,
    key_id: i64,
) -> Result<Vec<PersonalSshKeyGrant>> {
    let rows: Vec<GrantRow> = sqlx::query_as(
        "SELECT g.id, g.role_id, g.principal_id, p.kind AS principal_kind, \
             coalesce(p.name, r.name) AS grantee_name, \
             coalesce(p.foreign_id, r.foreign_id) AS grantee_foreign_id \
         FROM grants g \
         LEFT JOIN principals p ON p.id = g.principal_id \
         LEFT JOIN roles r ON r.id = g.role_id \
         WHERE g.ssh_key_secret_id = $1 ORDER BY g.id ASC",
    )
    .bind(key_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(PersonalSshKeyGrant::from).collect())
}

/// One grant of the personal key, with its resolved grantee.
#[derive(Debug, sqlx::FromRow)]
struct GrantRow {
    id: i64,
    role_id: Option<i64>,
    principal_id: Option<i64>,
    grantee_name: Option<String>,
    grantee_foreign_id: Option<String>,
    principal_kind: Option<String>,
}

impl From<GrantRow> for PersonalSshKeyGrant {
    fn from(row: GrantRow) -> Self {
        let grantee_oid = match (row.role_id, row.principal_id) {
            (Some(role_id), _) => model::Role::oid(role_id),
            (None, Some(principal_id)) => model::Principal::oid(principal_id),
            (None, None) => String::new(),
        };
        Self {
            id: row.id,
            role_id: row.role_id,
            principal_id: row.principal_id,
            grantee_oid,
            grantee_name: row.grantee_name,
            grantee_foreign_id: row.grantee_foreign_id,
            principal_kind: row.principal_kind,
        }
    }
}

/// `SshKeySecret`'s validations for a personal key write.
///
/// The rules are garde attributes on [`PersonalKeyRules`]; each value is what the
/// write path computed (the trimmed name, whether a key or source exists, and the
/// parse result `SshKey` reported).
fn personal_key_errors(
    name: &str,
    private_key: PrivateKeyCheck<'_>,
    public_key: Option<&str>,
) -> FieldErrors {
    PersonalKeyRules {
        name: name.to_owned(),
        private_key,
        public_key: public_key.map(str::to_owned),
    }
    .validate_fields()
    .err()
    .unwrap_or_default()
}

/// The private key a personal-key write would store, as the rules see it.
#[derive(Debug, Clone, Copy)]
enum PrivateKeyCheck<'a> {
    /// Nothing supplied and no stored source: `private_key` is blank.
    Missing,
    /// A supplied key `SshKey` rejected; its message is the attribute's error.
    /// Messages never include the submitted key.
    Invalid(&'a str),
    /// Supplied or already stored, and it parsed.
    Present,
}

impl PrivateKeyCheck<'_> {
    /// The parse failure, which also suppresses the public key's rule.
    fn error(&self) -> Option<&str> {
        match self {
            Self::Invalid(message) => Some(message),
            _ => None,
        }
    }
}

/// A personal key write's attributes: the trimmed name, the key decision the
/// write path computed, and the stored public key.
#[derive(garde::Validate)]
struct PersonalKeyRules<'a> {
    // A present name of at most 100 characters.
    #[garde(length(chars, min = 1, max = 100))]
    name: String,
    // A private key that is present and parses.
    #[garde(custom(key_rules::private_key_present_and_valid))]
    private_key: PrivateKeyCheck<'a>,
    // A supported public key, skipped for an unparsable key so only the private
    // key reports it.
    #[garde(custom(key_rules::public_key_supported(self.private_key.error())))]
    public_key: Option<String>,
}

/// The field rules garde has no built-in for.
mod key_rules {
    use super::{PUBLIC_KEY_FORMAT_MESSAGE, PrivateKeyCheck};
    use crate::validation::rules::BLANK_MESSAGE;

    /// The private key is present and parses.
    pub(super) fn private_key_present_and_valid(
        value: &PrivateKeyCheck<'_>,
        _: &(),
    ) -> garde::Result {
        match value {
            PrivateKeyCheck::Missing => Err(garde::Error::new(BLANK_MESSAGE)),
            PrivateKeyCheck::Invalid(message) => Err(garde::Error::new(*message)),
            PrivateKeyCheck::Present => Ok(()),
        }
    }

    /// A rename keeps the stored key, so its public key still has to pass.
    pub(super) fn public_key_supported(
        private_key_error: Option<&str>,
    ) -> impl FnOnce(&Option<String>, &()) -> garde::Result + '_ {
        move |value, _| {
            if private_key_error.is_none()
                && !value
                    .as_deref()
                    .is_some_and(crate::ssh_key::supported_public_key)
            {
                return Err(garde::Error::new(PUBLIC_KEY_FORMAT_MESSAGE));
            }
            Ok(())
        }
    }
}

/// Creates or replaces the caller's key. `submitted` is the pasted private key,
/// when the body sent one.
pub async fn save_personal_ssh_key(
    pool: &PgPool,
    encryption: &active_record_encryption::ActiveRecordEncryption,
    owner_id: i64,
    name: Option<&str>,
    submitted: Option<&str>,
) -> SkillWriteResult<PersonalSshKey> {
    // An empty value keeps the stored key.
    let derived = submitted
        .filter(|value| !value.is_empty())
        .map(|value| (value, crate::ssh_key::derive_public_key(value)));
    let private_key = match &derived {
        Some((value, Ok(public_key))) => Some((*value, public_key.as_str())),
        _ => None,
    };
    let private_key_error = match &derived {
        Some((_, Err(message))) => Some(message.as_str()),
        _ => None,
    };
    let mut tx = begin_write(pool).await?;
    let existing: Option<(i64, String, String)> = sqlx::query_as(
        "SELECT id, name, public_key FROM ssh_key_secrets WHERE owner_id = $1 FOR UPDATE",
    )
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await?;
    let name = name
        .map(str::to_owned)
        .or_else(|| existing.as_ref().map(|(_, name, _)| name.clone()))
        .unwrap_or_else(|| PERSONAL_SSH_KEY_DEFAULT_NAME.to_owned());
    let name = name.trim().to_owned();
    let stored_public_key = existing
        .as_ref()
        .map(|(_, _, public_key)| public_key.clone());
    let has_source = match &existing {
        Some((id, _, _)) => {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM secret_sources WHERE ssh_key_secret_id = $1)",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            exists
        }
        None => false,
    };
    // `SecretSource` invalidates the principals granted its owner whenever one of
    // its own attributes changes, and a re-submitted plaintext is not a change.
    // A new key has no grants yet, so only replacing an existing value bumps.
    let source_changed = match (&existing, private_key) {
        (Some((id, _, _)), Some((value, _))) => {
            let stored: Option<String> = sqlx::query_scalar(
                "SELECT secret FROM secret_sources WHERE ssh_key_secret_id = $1",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
            stored
                .map(|secret| encryption.decrypt(&secret))
                .transpose()?
                .as_deref()
                != Some(value)
        }
        _ => false,
    };
    // A submitted key that failed to parse still created (or replaced) a source,
    // so it is the source's own error rather than the blank one, and it
    // suppresses the public-key rule.
    let private_key_supplied = private_key.is_some() || private_key_error.is_some() || has_source;
    let private_key_check = match (private_key_supplied, private_key_error) {
        (false, _) => PrivateKeyCheck::Missing,
        (true, Some(message)) => PrivateKeyCheck::Invalid(message),
        (true, None) => PrivateKeyCheck::Present,
    };
    let errors = personal_key_errors(
        &name,
        private_key_check,
        // A rename keeps the stored key, so its public key still has to pass.
        private_key
            .map(|(_, public_key)| public_key)
            .or(stored_public_key.as_deref()),
    );
    if !errors.is_empty() {
        return Err(SkillWriteError::Invalid(errors));
    }
    let (public_key, secret) = private_key
        .map(|(value, public_key)| (public_key, Some(value)))
        .unwrap_or_default();
    match &existing {
        Some((id, _, _)) => {
            sqlx::query(
                "UPDATE ssh_key_secrets SET name = $2, public_key = coalesce($3, public_key), \
                     updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(&name)
            .bind(private_key.map(|(_, public_key)| public_key))
            .execute(&mut *tx)
            .await?;
            if let Some(secret) = secret {
                sqlx::query(
                    "UPDATE secret_sources SET secret = $2, updated_at = now() \
                     WHERE ssh_key_secret_id = $1",
                )
                .bind(id)
                .bind(encryption.encrypt(secret)?)
                .execute(&mut *tx)
                .await?;
            }
            if source_changed {
                // The source belongs to the key, so it invalidates every union of
                // principals reached by the key's direct and role grants.
                invalidate::invalidate(
                    &mut tx,
                    Scope::Secret {
                        kind: SecretKind::SshKey,
                        id: *id,
                    },
                )
                .await?;
            }
        }
        None => {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO ssh_key_secrets (name, public_key, labels, owner_id, created_by_id, \
                     created_at, updated_at) \
                 VALUES ($1, $2, '{}'::jsonb, $3, $3, now(), now()) RETURNING id",
            )
            .bind(&name)
            .bind(public_key)
            .bind(owner_id)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO secret_sources (ssh_key_secret_id, source_type, secret, config, \
                     created_at, updated_at) \
                 VALUES ($1, 'control_plane', $2, '{}'::jsonb, now(), now())",
            )
            .bind(id)
            .bind(encryption.encrypt(secret.unwrap_or_default())?)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    personal_ssh_key(pool, owner_id)
        .await?
        .ok_or(SkillWriteError::Missing)
}

/// Removes the key, its source, and every grant of it.
pub async fn delete_personal_ssh_key(pool: &PgPool, owner_id: i64) -> Result<()> {
    let mut tx = begin_write(pool).await?;
    let ids: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM ssh_key_secrets WHERE owner_id = $1 FOR UPDATE")
            .bind(owner_id)
            .fetch_all(&mut *tx)
            .await?;
    for id in ids {
        // Each destroyed grant invalidates its grantee.
        let grants: Vec<(Option<i64>, Option<i64>)> = sqlx::query_as(
            "DELETE FROM grants WHERE ssh_key_secret_id = $1 RETURNING principal_id, role_id",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        for (principal_id, role_id) in grants {
            invalidate::invalidate(
                &mut tx,
                Scope::Grantee {
                    principal_id,
                    role_id,
                },
            )
            .await?;
        }
        sqlx::query("DELETE FROM secret_sources WHERE ssh_key_secret_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM ssh_key_secrets WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Grants the caller's key to every principal that verifiably is the caller.
pub async fn apply_personal_ssh_key_grants(pool: &PgPool, user_id: i64) -> Result<usize> {
    let mut tx = begin_write(pool).await?;
    let key_id: Option<i64> = sqlx::query_scalar(
        "SELECT k.id FROM ssh_key_secrets k JOIN users u ON u.id = k.owner_id \
         WHERE k.owner_id = $1 AND u.status = 'active' FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(key_id) = key_id else {
        return Ok(0);
    };
    // `owned_principals`: one OR relation over the caller's Console principal
    // and the Slack DM that provably is the caller.
    let principal_ids: Vec<i64> =
        if let Some((subject, team_id)) = unambiguous_slack_identity(&mut tx, user_id).await? {
            sqlx::query_scalar(
                "SELECT id FROM principals WHERE \
                 (kind = 'console_user' AND console_user_id = $1) OR \
                 (kind = 'slack_dm' AND slack_user_id = $2 AND slack_team_id = $3)",
            )
            .bind(user_id)
            .bind(&subject)
            .bind(&team_id)
            .fetch_all(&mut *tx)
            .await?
        } else {
            sqlx::query_scalar(
                "SELECT id FROM principals WHERE kind = 'console_user' AND console_user_id = $1",
            )
            .bind(user_id)
            .fetch_all(&mut *tx)
            .await?
        };
    let mut granted = 0;
    for principal_id in principal_ids {
        let inserted = sqlx::query(
            "INSERT INTO grants (principal_id, ssh_key_secret_id, priority, created_by_id, \
                 created_at, updated_at) \
             SELECT $1, $2, $3, $4, now(), now() \
             WHERE NOT EXISTS (SELECT 1 FROM grants WHERE principal_id = $1 AND ssh_key_secret_id = $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(principal_id)
        .bind(key_id)
        .bind(DEFAULT_DIRECT_PRIORITY)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() > 0 {
            // `Grant`'s sync-config invalidation bumps its grantee; grants are
            // owned by the principals area, so reuse its invalidation.
            invalidate::invalidate(
                &mut tx,
                Scope::Grantee {
                    principal_id: Some(principal_id),
                    role_id: None,
                },
            )
            .await?;
            granted += 1;
        }
    }
    tx.commit().await?;
    Ok(granted)
}

/// `UserIdentity.unambiguous_slack_identity`: the user's one verified Slack
/// identity, when it looks like a Slack user/team pair.
async fn unambiguous_slack_identity(
    executor: &mut PgConnection,
    user_id: i64,
) -> Result<Option<(String, String)>> {
    let pairs: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT subject, team_id FROM user_identities \
         WHERE user_id = $1 AND provider = 'slack' \
           AND subject <> '' AND team_id IS NOT NULL AND team_id <> '' \
         ORDER BY subject, team_id",
    )
    .bind(user_id)
    .fetch_all(&mut *executor)
    .await?;
    if pairs.len() != 1 {
        return Ok(None);
    }
    let (subject, team_id) = pairs.into_iter().next().unwrap_or_default();
    if !slack_id(&subject, "UW") || !slack_id(&team_id, "TE") {
        return Ok(None);
    }
    Ok(Some((subject, team_id)))
}

/// The user-id and team-id formats.
fn slack_id(value: &str, prefixes: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    prefixes.contains(first)
        && value.len() >= 9
        && characters.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Sandbox permissions
// ---------------------------------------------------------------------------

/// A sandbox principal's identity and capability flags, as the sandbox
/// permissions payload reports them.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SandboxPrincipal {
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub kind: String,
    pub sandbox_repo_cache: String,
    pub sandbox_observability_enabled: bool,
    pub sandbox_sessions_read_enabled: bool,
    pub sandbox_workflows_read_enabled: bool,
    pub sandbox_workflows_write_enabled: bool,
}

/// The effective state of a sandbox principal.
#[derive(Debug, Clone)]
pub struct SandboxPermissions {
    /// The principal row and its capability flags.
    pub principal: SandboxPrincipal,
    /// The principal's effective channel permissions.
    pub slack_channel_permissions: Vec<SlackChannelPermissionPayload>,
    /// The principal's OAuth credentials.
    pub oauth_credentials: Vec<OauthCredentialPayload>,
    /// The tools the principal is connected to.
    pub connected_tools: Vec<String>,
    /// The redacted sync config the sandbox receives.
    pub permissions: PermissionsPayload,
}

/// One channel permission entry.
#[derive(Debug, Clone, Serialize)]
pub struct SlackChannelPermissionPayload {
    pub channel_id: String,
    pub upload_enabled: bool,
    pub download_enabled: bool,
    pub history_enabled: bool,
}

/// One OAuth credential entry.
#[derive(Debug, Clone, Serialize)]
pub struct OauthCredentialPayload {
    pub id: String,
    pub oauth_app_id: String,
    pub slug: String,
    pub provider: String,
    pub provider_email: Option<String>,
    pub provider_subject: Option<String>,
    pub status: &'static str,
    pub scopes: Value,
}

/// The redacted sync config a sandbox reports: the proxy configuration it
/// receives.
#[derive(Debug, Clone, Serialize)]
pub struct PermissionsPayload {
    pub secrets: Vec<Value>,
    pub transforms: Vec<Value>,
    pub postgres: Vec<Value>,
}

/// Every attribute the sandbox permissions payload reports.
pub async fn sandbox_permissions(
    pool: &PgPool,
    principal_id: i64,
) -> Result<Option<SandboxPermissions>> {
    let Some(principal) = sqlx::query_as(
        "SELECT id, foreign_id, name, kind, sandbox_repo_cache, \
                sandbox_observability_enabled, sandbox_sessions_read_enabled, \
                sandbox_workflows_read_enabled, sandbox_workflows_write_enabled \
         FROM principals WHERE id = $1",
    )
    .bind(principal_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let Some(config) = crate::principals::effective_config(pool, principal_id).await? else {
        return Ok(None);
    };
    Ok(Some(SandboxPermissions {
        principal,
        slack_channel_permissions: slack_channel_permission_payload(pool, principal_id).await?,
        oauth_credentials: oauth_credential_payload(pool, principal_id).await?,
        connected_tools: connected_tool_names(pool, principal_id).await?,
        permissions: PermissionsPayload {
            secrets: config.secrets,
            transforms: config.transforms,
            postgres: config.postgres,
        },
    }))
}

/// The principal's effective channel permissions: direct grants first,
/// then role grants, strongest flag winning, orderable by channel.
async fn slack_channel_permission_payload(
    pool: &PgPool,
    principal_id: i64,
) -> Result<Vec<SlackChannelPermissionPayload>> {
    let rows: Vec<SlackChannelPermissionRow> = sqlx::query_as(
        "SELECT channel_id, upload_enabled, download_enabled, history_enabled, \
                principal_id, role_id, id \
         FROM slack_channel_permissions \
         WHERE principal_id = $1 \
            OR role_id IN (SELECT role_id FROM principal_roles WHERE principal_id = $1) \
         ORDER BY (principal_id IS NOT NULL) DESC, coalesce(role_id, 0) ASC, id ASC",
    )
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    let mut merged: BTreeMap<String, [bool; 3]> = BTreeMap::new();
    for row in &rows {
        let entry = merged
            .entry(row.channel_id.trim().to_uppercase())
            .or_insert([false; 3]);
        for (slot, flag) in entry.iter_mut().zip([
            row.upload_enabled,
            row.download_enabled,
            row.history_enabled,
        ]) {
            *slot = *slot || flag;
        }
    }
    Ok(merged
        .into_iter()
        .map(
            |(channel_id, [upload_enabled, download_enabled, history_enabled])| {
                SlackChannelPermissionPayload {
                    channel_id,
                    upload_enabled,
                    download_enabled,
                    history_enabled,
                }
            },
        )
        .collect())
}

/// The columns of one `slack_channel_permissions` row, before merging.
#[derive(Debug, sqlx::FromRow)]
struct SlackChannelPermissionRow {
    channel_id: String,
    upload_enabled: bool,
    download_enabled: bool,
    history_enabled: bool,
}

/// The principal's OAuth credentials.
async fn oauth_credential_payload(
    pool: &PgPool,
    principal_id: i64,
) -> Result<Vec<OauthCredentialPayload>> {
    let rows: Vec<OauthCredentialRow> = sqlx::query_as(
        "WITH effective_grants AS ( \
             SELECT g.static_secret_id, g.priority FROM grants g \
             WHERE g.principal_id = $1 AND g.static_secret_id IS NOT NULL \
             UNION ALL \
             SELECT g.static_secret_id, g.priority FROM principal_roles pr \
             JOIN grants g ON g.role_id = pr.role_id \
             WHERE pr.principal_id = $1 AND g.static_secret_id IS NOT NULL \
         ), granted AS ( \
             SELECT static_secret_id, MAX(priority) AS priority \
             FROM effective_grants GROUP BY static_secret_id \
         ) \
         SELECT bc.id, bc.provider_email, bc.provider_subject, bc.dead, bc.last_refresh, \
                bc.scopes, oa.id AS oauth_app_id, oa.slug, oa.provider \
         FROM granted \
         JOIN static_secrets ss ON ss.id = granted.static_secret_id \
         JOIN broker_credentials bc ON bc.id = ss.broker_credential_id \
         JOIN oauth_apps oa ON oa.id = bc.oauth_app_id \
         ORDER BY oa.slug ASC, coalesce(bc.provider_email, '') ASC, bc.id ASC",
    )
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    let mut credentials = Vec::with_capacity(rows.len());
    let mut seen: HashSet<i64> = HashSet::new();
    for row in rows {
        if !seen.insert(row.id) {
            continue;
        }
        // The status derives from those columns.
        let status = if row.dead {
            "dead"
        } else if row.last_refresh.is_none() {
            "bootstrapping"
        } else {
            "live"
        };
        credentials.push(OauthCredentialPayload {
            id: model::BrokerCredential::oid(row.id),
            oauth_app_id: model::OauthApp::oid(row.oauth_app_id),
            slug: row.slug,
            provider: row.provider,
            provider_email: row.provider_email,
            provider_subject: row.provider_subject,
            status,
            scopes: row.scopes,
        });
    }
    Ok(credentials)
}

/// The columns [`oauth_credential_payload`] renders per granted credential.
#[derive(Debug, sqlx::FromRow)]
struct OauthCredentialRow {
    id: i64,
    oauth_app_id: i64,
    slug: String,
    provider: String,
    provider_email: Option<String>,
    provider_subject: Option<String>,
    dead: bool,
    last_refresh: Option<PrimitiveDateTime>,
    scopes: Value,
}

/// The `centaur-tool` label of every granted credential.
async fn connected_tool_names(pool: &PgPool, principal_id: i64) -> Result<Vec<String>> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "WITH effective_grants AS ( \
             SELECT * FROM grants WHERE principal_id = $1 \
             UNION ALL \
             SELECT g.* FROM principal_roles pr JOIN grants g ON g.role_id = pr.role_id \
             WHERE pr.principal_id = $1 \
         ) \
         SELECT labels FROM ( \
             SELECT ss.labels FROM effective_grants g \
             JOIN static_secrets ss ON ss.id = g.static_secret_id \
             UNION ALL SELECT gs.labels FROM effective_grants g \
             JOIN gcp_auth_secrets gs ON gs.id = g.gcp_auth_secret_id \
             UNION ALL SELECT gi.labels FROM effective_grants g \
             JOIN gcp_id_token_secrets gi ON gi.id = g.gcp_id_token_secret_id \
             UNION ALL SELECT aa.labels FROM effective_grants g \
             JOIN aws_auth_secrets aa ON aa.id = g.aws_auth_secret_id \
             UNION ALL SELECT ot.labels FROM effective_grants g \
             JOIN oauth_token_secrets ot ON ot.id = g.oauth_token_secret_id \
             UNION ALL SELECT pd.labels FROM effective_grants g \
             JOIN pg_dsn_secrets pd ON pd.id = g.pg_dsn_secret_id \
             UNION ALL SELECT hm.labels FROM effective_grants g \
             JOIN hmac_secrets hm ON hm.id = g.hmac_secret_id \
             UNION ALL SELECT sk.labels FROM effective_grants g \
             JOIN ssh_key_secrets sk ON sk.id = g.ssh_key_secret_id \
         ) grantables",
    )
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    let mut tools: Vec<String> = rows
        .iter()
        .filter_map(|labels| labels.get("centaur-tool").and_then(Value::as_str))
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
        .collect();
    tools.sort();
    tools.dedup();
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::validation::ValidateFields;

    #[test]
    fn workflow_schedule_labels_read_the_tagged_kind() {
        assert_eq!(
            workflow_schedule_label(&json!({ "kind": { "type": "cron", "cron": "0 9 * * *" } })),
            Some("cron 0 9 * * *".to_owned())
        );
        assert_eq!(
            workflow_schedule_label(
                &json!({ "kind": { "type": "interval", "interval_seconds": 300 } })
            ),
            Some("every 5m".to_owned())
        );
        assert_eq!(
            workflow_schedule_label(
                &json!({ "kind": { "type": "interval", "interval_seconds": 90 } })
            ),
            Some("every 90s".to_owned())
        );
        assert_eq!(
            workflow_schedule_label(&json!({ "kind": { "type": "unknown" } })),
            None
        );
        assert_eq!(workflow_schedule_label(&json!({})), None);
    }

    fn keys(errors: FieldErrors) -> Vec<String> {
        errors
            .entries()
            .map(|(attribute, _)| attribute.to_owned())
            .collect()
    }

    fn candidate() -> Candidate {
        Candidate {
            name: Some("Digest".to_owned()),
            prompt: "Summarize the channel".to_owned(),
            delivery_channel: "C0DIFF1234".to_owned(),
            cron_expression: Some("0 9 * * *".to_owned()),
            timezone: Some(DEFAULT_TIMEZONE.to_owned()),
            enabled: true,
            next_run_at: None,
        }
    }

    #[test]
    fn a_valid_task_candidate_passes_the_field_rules() {
        assert!(candidate().validate_fields().is_ok());
        let mut without_schedule = candidate();
        without_schedule.cron_expression = None;
        assert!(without_schedule.validate_fields().is_ok());
    }

    #[test]
    fn invalid_task_candidates_are_rejected() {
        let cases: [fn(&mut Candidate); 9] = [
            |candidate| candidate.name = Some(String::new()),
            |candidate| candidate.name = None,
            |candidate| candidate.prompt = String::new(),
            |candidate| candidate.delivery_channel = String::new(),
            |candidate| candidate.delivery_channel = "not-a-channel".to_owned(),
            |candidate| candidate.timezone = Some("Mars/Olympus".to_owned()),
            |candidate| candidate.cron_expression = Some("every day".to_owned()),
            |candidate| candidate.name = Some("n".repeat(101)),
            |candidate| candidate.prompt = "p".repeat(65537),
        ];
        for invalidate in cases {
            let mut candidate = candidate();
            invalidate(&mut candidate);
            assert!(
                candidate.validate_fields().is_err(),
                "candidate should be rejected: {candidate:?}"
            );
        }
    }

    fn skill_attributes() -> SkillAttributes<'static> {
        SkillAttributes {
            content: "# Docs",
            name: "diff-skill",
            description: "A skill",
            visibility: None,
        }
    }

    #[test]
    fn a_valid_skill_passes_the_field_rules() {
        assert!(skill_attributes().validate_fields().is_ok());
        let private = SkillAttributes {
            visibility: Some("private"),
            ..skill_attributes()
        };
        assert!(private.validate_fields().is_ok());
    }

    #[test]
    fn invalid_skill_attributes_are_rejected() {
        for (name, description, visibility) in [
            ("", "A skill", None),
            ("Not Kebab", "A skill", None),
            ("a--b", "A skill", None),
            ("search", "A skill", None),
            ("diff-skill", "", None),
            ("diff-skill", "A skill", Some("nope")),
        ] {
            let attributes = SkillAttributes {
                content: "# Docs",
                name,
                description,
                visibility,
            };
            assert!(
                attributes.validate_fields().is_err(),
                "attributes should be rejected: {attributes:?}"
            );
        }
        let long_name = "n".repeat(65);
        let attributes = SkillAttributes {
            name: &long_name,
            ..skill_attributes()
        };
        assert!(attributes.validate_fields().is_err());
        let empty_content = SkillAttributes {
            content: "",
            ..skill_attributes()
        };
        assert!(empty_content.validate_fields().is_err());
    }

    #[test]
    fn a_skill_document_over_64_kib_is_a_content_error() {
        let content = "c".repeat(SKILL_MAX_DOCUMENT_BYTES);
        let attributes = SkillAttributes {
            content: &content,
            ..skill_attributes()
        };
        assert_eq!(keys(attributes.validate_fields().unwrap_err()), ["content"]);
    }

    #[test]
    fn a_blank_personal_key_reports_its_three_attributes() {
        let errors = personal_key_errors("", PrivateKeyCheck::Missing, None);
        let keys = keys(errors);
        assert!(keys.contains(&"name".to_owned()));
        assert!(keys.contains(&"private_key".to_owned()));
        assert!(keys.contains(&"public_key".to_owned()));
    }

    #[test]
    fn an_oversized_personal_key_name_is_rejected() {
        let name = "n".repeat(101);
        let errors = personal_key_errors(&name, PrivateKeyCheck::Present, Some("ssh-ed25519 AAAA"));
        assert_eq!(keys(errors), ["name"]);
    }

    #[test]
    fn an_unparsable_private_key_is_the_only_key_error() {
        // `SshKey`'s message never includes the submitted key.
        let errors = personal_key_errors(
            "My SSH key",
            PrivateKeyCheck::Invalid("private key is not an OpenSSH key"),
            None,
        );
        assert_eq!(keys(errors), ["private_key"]);
    }
}
