//! Scheduled-task endpoints: the console pages' API (a user's own tasks),
//! the sandbox API (the tasks of the sandbox's console user, or of its Slack
//! channel), and the bearer execution-state lookup.
//!
//! After a task is created, edited, or deleted the console hints api-rs to
//! reconcile it; "run now" asks api-rs to spawn a manual tick.

use std::collections::HashMap;

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use centaur_console_auth::{AuthUser, sandbox::SandboxAuth};
use centaur_console_db::{decode_oid, oid, schedule};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::{
    delivery,
    tasks::{self, Edit, NewTask, Owner, Task},
};
use crate::{
    ApiError, ApiResult, AppState,
    auth::{BearerCaller, ConsoleCaller, SandboxCaller},
    extract::{Data, Path, Query, data},
};

use crate::misc::slack::{self, Catalog};

const MAX_CHANNEL_OPTIONS: i64 = 20;

/// `Some(None)` for an explicit `null`, `None` when the key is absent.
fn explicit_null<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// --- console ----------------------------------------------------------------

#[derive(Default, Deserialize)]
pub struct ConsoleInput {
    name: Option<String>,
    prompt: Option<String>,
    enabled: Option<bool>,
    delivery_mode: Option<String>,
    delivery_channel: Option<String>,
    schedule_preset: Option<String>,
    cron_expression: Option<String>,
    custom_time: Option<String>,
    custom_days: Option<Vec<String>>,
}

/// What the console form's fields mean for the task.
async fn console_edit(state: &AppState, user: &AuthUser, input: ConsoleInput) -> ApiResult<Edit> {
    let mut edit = Edit {
        name: input.name.as_deref().map(tasks::normalize_name),
        prompt: input.prompt.as_deref().map(tasks::normalize_prompt),
        enabled: input.enabled,
        ..Edit::default()
    };
    if let Some(preset) = &input.schedule_preset {
        let cron = schedule::cron_for_choice(
            preset,
            input.cron_expression.as_deref(),
            input.custom_days.as_deref().unwrap_or_default(),
            input.custom_time.as_deref(),
        );
        edit.cron_expression = Some(tasks::normalize_cron(cron.as_deref()));
    }
    if input.delivery_mode.is_some() || input.delivery_channel.is_some() {
        let mode = input
            .delivery_mode
            .as_deref()
            .map(str::trim)
            .filter(|mode| !mode.is_empty())
            .unwrap_or("channel");
        let channel = match mode {
            "dm" => {
                let mut conn = state.db.acquire().await?;
                delivery::slack_identity(&mut conn, user.id)
                    .await?
                    .map(|identity| identity.user_id)
            }
            "channel" => input.delivery_channel,
            _ => None,
        };
        edit.delivery_channel = Some(channel.unwrap_or_default().trim().to_owned());
    }
    Ok(edit)
}

/// The caller's own view of tasks: their Slack DM and channel names.
struct View {
    author_oid: String,
    dm_user_id: Option<String>,
    names: HashMap<String, String>,
}

impl View {
    async fn load(state: &AppState, user: &AuthUser, tasks: &[&Task]) -> ApiResult<Self> {
        let mut conn = state.db.acquire().await?;
        let dm_user_id = delivery::slack_identity(&mut conn, user.id)
            .await?
            .map(|identity| identity.user_id);
        let mut ids: Vec<String> = tasks
            .iter()
            .map(|task| task.delivery_channel.clone())
            .filter(|id| !id.trim().is_empty())
            .collect();
        ids.sort();
        ids.dedup();
        let names = if ids.is_empty() {
            HashMap::new()
        } else {
            match delivery::channel_names(&mut *conn, &ids).await {
                Ok(rows) => rows.into_iter().collect(),
                Err(error) => {
                    tracing::warn!(%error, "scheduled_task_slack_channels_load_failed");
                    HashMap::new()
                }
            }
        };
        Ok(Self {
            author_oid: oid("usr", user.id),
            dm_user_id,
            names,
        })
    }

    fn payload(&self, task: &Task, is_new: bool) -> Value {
        let channel = task.delivery_channel.trim();
        let present = !channel.is_empty();
        let delivery_mode =
            if channel.starts_with(['U', 'W']) || (!present && self.dm_user_id.is_some()) {
                "dm"
            } else {
                "channel"
            };
        let delivery_name = if present && self.dm_user_id.as_deref() == Some(channel) {
            Some("Direct message to you".to_owned())
        } else if is_new && !present {
            None
        } else {
            self.names
                .get(&task.delivery_channel)
                .map(|name| format!("#{name}"))
        };
        let cron = task.cron_expression.as_deref();
        let weekly = schedule::weekly(cron);
        let existing = |value: Value| if is_new { Value::Null } else { value };
        json!({
            "id": existing(json!(task.oid())),
            "name": existing(json!(task.name)),
            "prompt": existing(json!(task.prompt)),
            "author_id": self.author_oid,
            "principal_id": task.principal_id.map(|id| oid("prn", id)),
            "delivery_channel": present.then_some(&task.delivery_channel),
            "delivery_mode": delivery_mode,
            "delivery_name": delivery_name,
            "direct_message_user_id": self.dm_user_id,
            "cron_expression": task.cron_expression,
            "timezone": task.timezone,
            "schedule_preset": schedule::preset(cron),
            "schedule_label": schedule::label(cron),
            "custom_days": weekly.as_ref().map(|weekly| weekly.days.clone()).unwrap_or_default(),
            "custom_time": weekly.map_or_else(|| "09:00".to_owned(), |weekly| weekly.time),
            "enabled": task.enabled,
            "next_run_at": task.next_run_at,
            "last_enqueued_at": task.last_enqueued_at,
            "last_run_id": task.last_run_id,
            "last_run_at": task.last_run_at,
            "last_error": task.last_error,
            "created_at": existing(json!(task.created_at)),
            "updated_at": existing(json!(task.updated_at)),
        })
    }
}

pub async fn console_index(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
) -> ApiResult {
    let all = tasks::list_owned(&state, &Owner::Author(user.id)).await?;
    let view = View::load(&state, &user, &all.iter().collect::<Vec<_>>()).await?;
    let rows: Vec<Value> = all.iter().map(|task| view.payload(task, false)).collect();
    Ok(data(rows).into_response())
}

/// A blank form: a daily 9:00 schedule.
pub async fn console_new(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
) -> ApiResult {
    let view = View::load(&state, &user, &[]).await?;
    let blank = Task {
        id: 0,
        author_id: Some(user.id),
        principal_id: None,
        name: String::new(),
        prompt: String::new(),
        delivery_channel: String::new(),
        cron_expression: Some("0 9 * * *".into()),
        timezone: schedule::DEFAULT_TIMEZONE.into(),
        enabled: true,
        next_run_at: None,
        last_enqueued_at: None,
        last_run_id: None,
        last_run_at: None,
        last_error: None,
        created_at: state.now(),
        updated_at: state.now(),
    };
    Ok(data(view.payload(&blank, true)).into_response())
}

pub async fn console_edit_form(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let task = tasks::find_owned(&state, &Owner::Author(user.id), &id).await?;
    let view = View::load(&state, &user, &[&task]).await?;
    Ok(data(view.payload(&task, false)).into_response())
}

pub async fn console_create(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Data(input): Data<ConsoleInput>,
) -> ApiResult {
    let edit = console_edit(&state, &user, input).await?;
    let task = tasks::create(
        &state,
        &Owner::Author(user.id),
        NewTask {
            name: edit.name.unwrap_or_default(),
            prompt: edit.prompt.unwrap_or_default(),
            delivery_channel: edit.delivery_channel.unwrap_or_default(),
            cron_expression: edit.cron_expression.flatten(),
            enabled: edit.enabled.unwrap_or(true),
        },
    )
    .await?;
    tasks::notify_reconcile(&state, &task.oid()).await;
    let view = View::load(&state, &user, &[&task]).await?;
    Ok((StatusCode::CREATED, data(view.payload(&task, false))).into_response())
}

pub async fn console_update(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Path(id): Path<String>,
    Data(input): Data<ConsoleInput>,
) -> ApiResult {
    let edit = console_edit(&state, &user, input).await?;
    let task = tasks::update(&state, &Owner::Author(user.id), &id, edit).await?;
    tasks::notify_reconcile(&state, &task.oid()).await;
    let view = View::load(&state, &user, &[&task]).await?;
    Ok(data(view.payload(&task, false)).into_response())
}

pub async fn console_destroy(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let task = tasks::delete(&state, &Owner::Author(user.id), &id).await?;
    tasks::notify_reconcile(&state, &task.oid()).await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub async fn console_run(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let task = tasks::find_owned(&state, &Owner::Author(user.id), &id).await?;
    if !task.enabled {
        return Err(ApiError::Unprocessable {
            message: "Enable the task before running it.".into(),
            reason: Some("task_disabled"),
        });
    }
    if !tasks::queue_manual_run(&state, &task.oid()).await {
        return Err(ApiError::upstream(
            StatusCode::SERVICE_UNAVAILABLE,
            "scheduled task run could not be queued",
            None,
        ));
    }
    let view = View::load(&state, &user, &[&task]).await?;
    let mut payload = view.payload(&task, false);
    payload["queued"] = json!(true);
    Ok((StatusCode::ACCEPTED, data(payload)).into_response())
}

#[derive(Deserialize)]
pub struct ChannelQuery {
    q: Option<String>,
    task_id: Option<String>,
}

/// Delivery channel search under the caller's delivery policy. `task_id`
/// (when editing) must be one of the caller's tasks.
pub async fn console_channel_options(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Query(query): Query<ChannelQuery>,
) -> ApiResult {
    if let Some(task_id) = query.task_id.as_deref().filter(|id| !id.trim().is_empty()) {
        tasks::find_owned(&state, &Owner::Author(user.id), task_id).await?;
    }
    let mut conn = state.db.acquire().await?;
    let identity = delivery::slack_identity(&mut conn, user.id).await?;
    let configured = slack::configured(&state.config.env);
    if configured {
        Catalog::from_state(&state).enqueue_if_empty().await;
    }
    let channels = delivery::allowed_channels(
        &mut conn,
        identity.as_ref(),
        query.q.as_deref().unwrap_or_default(),
        MAX_CHANNEL_OPTIONS,
    )
    .await?;
    let catalog_error = if !configured {
        Some("SLACK_BOT_TOKEN is not configured.")
    } else if delivery::catalog_empty(&mut *conn).await? {
        Some("Slack channel catalog is loading. Enter a channel ID or reload shortly.")
    } else if let Some(identity) = &identity
        && delivery::memberships_loading(&mut *conn, &identity.team_id).await?
    {
        Some("Slack channel memberships are loading.")
    } else {
        None
    };
    let options: Vec<Value> = channels
        .iter()
        .map(|channel| {
            json!({
                "channel_id": channel.channel_id,
                "name": channel.name,
                "private": channel.private,
                "label": format!("#{}", channel.name),
                "description": format!(
                    "{} · {}",
                    channel.channel_id,
                    if channel.private { "Private" } else { "Public" }
                ),
            })
        })
        .collect();
    Ok(
        axum::Json(json!({"data": options, "meta": {"catalog_error": catalog_error}}))
            .into_response(),
    )
}

// --- bearer -----------------------------------------------------------------

/// `GET /api/v1/scheduled_tasks/{id}`: the execution state api-rs needs.
pub async fn bearer_show(
    State(state): State<AppState>,
    BearerCaller(_): BearerCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let task_id = decode_oid(tasks::OID_PREFIX, &id).ok_or_else(ApiError::not_found)?;
    let row: Option<(bool, String)> =
        sqlx::query_as("SELECT enabled, delivery_channel FROM scheduled_tasks WHERE id = $1")
            .bind(task_id)
            .fetch_optional(&state.db)
            .await?;
    let (enabled, delivery_channel) = row.ok_or_else(ApiError::not_found)?;
    Ok(
        data(json!({"id": id, "enabled": enabled, "delivery_channel": delivery_channel}))
            .into_response(),
    )
}

// --- sandbox ----------------------------------------------------------------

#[derive(Default, Deserialize)]
pub struct SandboxInput {
    name: Option<String>,
    prompt: Option<String>,
    #[serde(default, deserialize_with = "explicit_null")]
    delivery_channel: Option<Option<String>>,
    #[serde(default, deserialize_with = "explicit_null")]
    cron_expression: Option<Option<String>>,
    enabled: Option<bool>,
}

struct SandboxOwner {
    owner: Owner,
    /// The Slack user whose request a channel task is made for.
    requester: Option<i64>,
}

/// The owner of the sandbox's tasks: a Slack channel session (on behalf of a
/// Slack user of the bot's workspace), or the console user its principal is
/// linked to.
async fn sandbox_owner(state: &AppState, auth: &SandboxAuth) -> ApiResult<SandboxOwner> {
    if auth.principal_kind == "slack_channel" {
        let (channel_id, team_id, requester_id): (Option<String>, Option<String>, Option<i64>) =
            sqlx::query_as(
                "SELECT p.slack_channel_id, p.slack_team_id, px.requester_principal_id \
                 FROM principals p, proxies px WHERE p.id = $1 AND px.id = $2",
            )
            .bind(auth.principal_id)
            .bind(auth.proxy_id)
            .fetch_one(&state.db)
            .await?;
        let requester: Option<(i64,)> = match requester_id {
            Some(id) => {
                sqlx::query_as(
                    "SELECT r.id FROM principals r WHERE r.id = $1 AND r.kind = 'slack_dm' \
                     AND btrim(COALESCE(r.slack_user_id, '')) <> '' \
                     AND btrim(COALESCE(r.slack_team_id, '')) <> '' \
                     AND EXISTS (SELECT 1 FROM slack_bot_channels c WHERE c.team_id = r.slack_team_id)",
                )
                .bind(id)
                .fetch_optional(&state.db)
                .await?
            }
            None => None,
        };
        return match requester {
            Some((requester,)) => Ok(SandboxOwner {
                owner: Owner::Channel {
                    principal_id: auth.principal_id,
                    channel_id: channel_id.map(|id| id.trim().to_owned()),
                    team_id,
                },
                requester: Some(requester),
            }),
            None => Err(ApiError::Forbidden(
                "channel tasks require a Slack user request".into(),
            )),
        };
    }
    let user = match auth.console_user_id {
        Some(id) => AuthUser::find(&state.db, id).await?,
        None => None,
    };
    match user {
        Some(user) if user.is_active() => Ok(SandboxOwner {
            owner: Owner::Author(user.id),
            requester: None,
        }),
        _ => Err(ApiError::Forbidden(
            "sandbox principal is not linked to an active Console user".into(),
        )),
    }
}

/// Resolves the owner and logs channel requests with their outcome.
async fn sandbox_call<F, Fut>(state: &AppState, auth: &SandboxAuth, action: &str, f: F) -> ApiResult
where
    F: FnOnce(Owner) -> Fut,
    Fut: std::future::Future<Output = ApiResult>,
{
    let resolved = sandbox_owner(state, auth).await;
    let requester = resolved.as_ref().ok().and_then(|owner| owner.requester);
    let result = match resolved {
        Ok(resolved) => f(resolved.owner).await,
        Err(error) => Err(error),
    };
    if auth.principal_kind == "slack_channel" {
        tracing::info!(
            principal = %oid("prn", auth.principal_id),
            requester = ?requester.map(|id| oid("prn", id)),
            action,
            status = match &result {
                Ok(response) => response.status().as_u16(),
                Err(error) => error.status().as_u16(),
            },
            "sandbox_channel_scheduled_task_request"
        );
    }
    result
}

fn sandbox_payload(task: &Task) -> Value {
    json!({
        "id": task.oid(),
        "name": task.name,
        "prompt": task.prompt,
        "delivery_channel": task.delivery_channel,
        "cron_expression": task.cron_expression,
        "timezone": task.timezone,
        "schedule_label": tasks::schedule_label(task),
        "enabled": task.enabled,
        "next_run_at": task.next_run_at,
        "last_enqueued_at": task.last_enqueued_at,
        "last_run_id": task.last_run_id,
        "last_run_at": task.last_run_at,
        "last_error": task.last_error,
        "created_at": task.created_at,
        "updated_at": task.updated_at,
    })
}

/// The delivery channel a sandbox request means. Channel tasks always
/// deliver to their channel; a user's tasks deliver to `dm` (their Slack DM,
/// also the default when creating) or the given destination.
async fn sandbox_delivery(
    state: &AppState,
    owner: &Owner,
    requested: &Option<Option<String>>,
    creating: bool,
) -> ApiResult<Option<String>> {
    let requested_text = requested
        .as_ref()
        .and_then(|value| value.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match owner {
        Owner::Channel { channel_id, .. } => {
            let matches_channel = requested_text
                .zip(channel_id.as_deref())
                .is_none_or(|(requested, channel)| requested.eq_ignore_ascii_case(channel));
            if matches_channel {
                Ok(Some(channel_id.clone().unwrap_or_default()))
            } else {
                Err(ApiError::Unprocessable {
                    message:
                        "tasks created in a Slack channel thread always deliver to that channel"
                            .into(),
                    reason: None,
                })
            }
        }
        Owner::Author(_) => {
            let wants_dm = requested_text.is_some_and(|value| value.eq_ignore_ascii_case("dm"))
                || (creating && requested.is_none());
            if wants_dm {
                let mut conn = state.db.acquire().await?;
                let identity = owner.identity(&mut conn).await?;
                Ok(Some(
                    identity
                        .map(|identity| identity.user_id)
                        .unwrap_or_default(),
                ))
            } else {
                Ok(requested
                    .as_ref()
                    .map(|value| value.as_deref().unwrap_or_default().trim().to_owned()))
            }
        }
    }
}

pub async fn sandbox_index(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
) -> ApiResult {
    let state = &state;
    sandbox_call(state, &auth, "index", |owner| async move {
        let all = tasks::list_owned(state, &owner).await?;
        let rows: Vec<Value> = all.iter().map(sandbox_payload).collect();
        Ok(data(rows).into_response())
    })
    .await
}

pub async fn sandbox_show(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let state = &state;
    sandbox_call(state, &auth, "show", |owner| async move {
        let task = tasks::find_owned(state, &owner, &id).await?;
        Ok(data(sandbox_payload(&task)).into_response())
    })
    .await
}

pub async fn sandbox_create(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Data(input): Data<SandboxInput>,
) -> ApiResult {
    let state = &state;
    sandbox_call(state, &auth, "create", |owner| async move {
        let delivery_channel = sandbox_delivery(state, &owner, &input.delivery_channel, true)
            .await?
            .unwrap_or_default();
        let task = tasks::create(
            state,
            &owner,
            NewTask {
                name: tasks::normalize_name(input.name.as_deref().unwrap_or_default()),
                prompt: tasks::normalize_prompt(input.prompt.as_deref().unwrap_or_default()),
                delivery_channel: delivery_channel.trim().to_owned(),
                cron_expression: tasks::normalize_cron(input.cron_expression.flatten().as_deref()),
                enabled: input.enabled.unwrap_or(true),
            },
        )
        .await?;
        tasks::notify_reconcile(state, &task.oid()).await;
        Ok((StatusCode::CREATED, data(sandbox_payload(&task))).into_response())
    })
    .await
}

pub async fn sandbox_update(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
    Data(input): Data<SandboxInput>,
) -> ApiResult {
    let state = &state;
    sandbox_call(state, &auth, "update", |owner| async move {
        let delivery_channel = match (&owner, &input.delivery_channel) {
            (Owner::Author(_), None) => None,
            _ => sandbox_delivery(state, &owner, &input.delivery_channel, false).await?,
        };
        let edit = Edit {
            name: input.name.as_deref().map(tasks::normalize_name),
            prompt: input.prompt.as_deref().map(tasks::normalize_prompt),
            delivery_channel: delivery_channel.map(|channel| channel.trim().to_owned()),
            cron_expression: input
                .cron_expression
                .map(|cron| tasks::normalize_cron(cron.as_deref())),
            enabled: input.enabled,
        };
        let task = tasks::update(state, &owner, &id, edit).await?;
        tasks::notify_reconcile(state, &task.oid()).await;
        Ok(data(sandbox_payload(&task)).into_response())
    })
    .await
}

pub async fn sandbox_destroy(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let state = &state;
    sandbox_call(state, &auth, "destroy", |owner| async move {
        let task = tasks::delete(state, &owner, &id).await?;
        tasks::notify_reconcile(state, &task.oid()).await;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

pub async fn sandbox_run(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let state = &state;
    sandbox_call(state, &auth, "run", |owner| async move {
        let task = tasks::find_owned(state, &owner, &id).await?;
        if !task.enabled {
            return Err(ApiError::Unprocessable {
                message: "scheduled task is disabled".into(),
                reason: None,
            });
        }
        if !tasks::queue_manual_run(state, &task.oid()).await {
            return Err(ApiError::upstream(
                StatusCode::SERVICE_UNAVAILABLE,
                "scheduled task run could not be queued",
                None,
            ));
        }
        let mut payload = sandbox_payload(&task);
        payload["queued"] = json!(true);
        Ok((StatusCode::ACCEPTED, data(payload)).into_response())
    })
    .await
}
