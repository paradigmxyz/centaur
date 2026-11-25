//! Console area: automation.
//!
//! Ports the scheduled-task surfaces (console, bearer, and sandbox), the
//! sandbox permissions snapshot, personal SSH keys, and skill authoring. The
//! database work lives in [`centaur_console_db::areas::automation`]; the
//! handlers here only turn HTTP into those calls and back.

// A fallible handler's error type is the same `Response` as its success type.
#![allow(clippy::result_large_err)]

use active_record_encryption::ActiveRecordEncryption;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use centaur_console_db::{
    ConsoleUser,
    ConsoleUserStatus,
    FieldErrors,
    Principal,
    areas::automation as db,
    model::{
        self, ConsoleModel, ScheduledTask as ScheduledTaskModel, Skill as SkillModel,
        User as UserModel,
    },
    scheduled_tasks::{self, ManualRun, RunRequest, StartError},
    // Shared with the bearer `ssh_key_secrets` area so the two Console SSH key
    // paths cannot drift.
    ssh_key::{derive_public_key, fingerprint},
    validation::ValidateFields,
};
use centaur_workflows::CreateWorkflowRunRequest;
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    ApiError, AppState,
    console::{
        guard::GuardedRoutes,
        http::{ConsoleError, DataPayload, json_data},
        request::{ConsoleData, ConsoleQuery, nullable_field},
        sandbox::SandboxContext,
    },
};

/// Routes under `/api/v1/console` (paths relative to that prefix).
pub(crate) fn console(routes: GuardedRoutes) -> GuardedRoutes {
    routes
        .console_user("/scheduled_tasks", get(console_index).post(console_create))
        .console_user("/scheduled_tasks/new", get(console_new))
        .console_user(
            "/scheduled_tasks/slack_channel_options",
            get(console_channel_options),
        )
        .console_user("/scheduled_tasks/{id}/edit", get(console_edit))
        .console_user(
            "/scheduled_tasks/{id}",
            axum::routing::patch(console_update).delete(console_destroy),
        )
        .console_user("/scheduled_tasks/{id}/run", post(console_run))
        .console_user("/skills", get(skills_index).post(skills_create))
        .console_user("/skills/editor_candidates", get(skill_editor_candidates))
        .console_user(
            "/skills/{id}",
            get(skills_show).patch(skills_update).delete(skills_destroy),
        )
        .console_user("/skills/{id}/share", post(skills_share))
        .console_user("/skills/{id}/unshare", post(skills_unshare))
        .console_user(
            "/me/ssh_key",
            get(ssh_key_show)
                .put(ssh_key_update)
                .delete(ssh_key_destroy),
        )
        .console_user("/me/ssh_key/validate", post(ssh_key_validate))
        .console_admin("/workflows", get(workflows_index))
        .console_admin("/workflows/{id}", get(workflows_show))
        .console_admin(
            "/workflows/{id}/runs",
            get(workflows_runs).post(workflows_create_run),
        )
}

/// Routes under `/api/v1` (bearer API; paths relative to that prefix).
pub(crate) fn bearer(routes: GuardedRoutes) -> GuardedRoutes {
    routes.operator("/scheduled_tasks/{id}", get(bearer_show))
}

/// Routes under `/api/v1/sandbox` (paths relative to that prefix).
pub(crate) fn sandbox(routes: GuardedRoutes) -> GuardedRoutes {
    use crate::auth::Capability;
    routes
        .sandbox(
            "/scheduled_tasks",
            get(sandbox_index).post(sandbox_create),
            Capability::SandboxTasks,
        )
        .sandbox(
            "/scheduled_tasks/{id}",
            get(sandbox_show)
                .patch(sandbox_update)
                .delete(sandbox_destroy),
            Capability::SandboxTasks,
        )
        .sandbox(
            "/scheduled_tasks/{id}/run",
            post(sandbox_run),
            Capability::SandboxTasks,
        )
        .sandbox(
            "/permissions",
            get(sandbox_permissions_show),
            Capability::SandboxPermissions,
        )
        .sandbox(
            "/skills",
            get(sandbox_skills_index).post(sandbox_skills_create),
            Capability::SandboxSkills,
        )
        .sandbox(
            "/skills/search",
            get(sandbox_skills_search),
            Capability::SandboxSkills,
        )
        .sandbox(
            "/skills/{id}",
            get(sandbox_skills_show)
                .patch(sandbox_skills_update)
                .delete(sandbox_skills_destroy),
            Capability::SandboxSkills,
        )
        .sandbox(
            "/skills/{id}/share",
            post(sandbox_skills_share),
            Capability::SandboxSkills,
        )
        .sandbox(
            "/skills/{id}/unshare",
            post(sandbox_skills_unshare),
            Capability::SandboxSkills,
        )
        .sandbox(
            "/skills/{id}/editors",
            get(sandbox_skills_editors)
                .post(sandbox_skills_add_editor)
                .delete(sandbox_skills_remove_editor),
            Capability::SandboxSkills,
        )
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn pool(state: &AppState) -> Result<sqlx::PgPool, ApiError> {
    state.console_session_db()
}

/// Console's 404 for a record the caller may not see: the model descriptor's
/// plain message, never SQL-ish relation text.
fn record_not_found(model: centaur_console_db::Error) -> Response {
    ConsoleError::from(model).into_response()
}

fn invalid(errors: FieldErrors) -> Response {
    ConsoleError::from(errors).into_response()
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().expect("valid header"));
    response
}

// ---------------------------------------------------------------------------
// Console: /api/v1/console/scheduled_tasks
// ---------------------------------------------------------------------------

async fn console_index(
    State(state): State<AppState>,
    user: ConsoleUser,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let tasks = db::list_author_tasks(&pool, user.id)
        .await
        .map_err(internal)?;
    let payloads = db::console_payloads(&pool, &tasks)
        .await
        .map_err(internal)?;
    Ok(no_store(
        Json(DataPayload { data: payloads }).into_response(),
    ))
}

async fn console_new(
    State(state): State<AppState>,
    user: ConsoleUser,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let policy = db::slack_policy_for_user(&pool, user.id)
        .await
        .map_err(internal)?;
    Ok(no_store(
        Json(DataPayload {
            data: db::new_task_payload(&UserModel::oid(user.id), policy.slack_user_id.as_deref()),
        })
        .into_response(),
    ))
}

async fn console_edit(
    State(state): State<AppState>,
    user: ConsoleUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let task = match ScheduledTaskModel::decode_oid(&id) {
        Some(task_id) => db::find_author_task(&pool, user.id, task_id)
            .await
            .map_err(internal)?,
        None => None,
    };
    let Some(task) = task else {
        return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
    };
    let payload = db::console_payload_for(&pool, &task)
        .await
        .map_err(internal)?;
    Ok(no_store(
        Json(DataPayload { data: payload }).into_response(),
    ))
}

async fn console_create(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleData(data): ConsoleData<ConsoleTaskData>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let draft = console_draft(&pool, user.id, &data).await?;
    let write = db::create_task(&pool, db::TaskOwner::Author(user.id), draft)
        .await
        .map_err(internal)?;
    Ok(no_store(
        write_response(&pool, write, StatusCode::CREATED).await?,
    ))
}

async fn console_update(
    State(state): State<AppState>,
    user: ConsoleUser,
    Path(id): Path<String>,
    ConsoleData(data): ConsoleData<ConsoleTaskData>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let Some(task_id) = ScheduledTaskModel::decode_oid(&id) else {
        return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
    };
    let draft = console_draft(&pool, user.id, &data).await?;
    let write = db::update_task(&pool, db::TaskOwner::Author(user.id), task_id, draft)
        .await
        .map_err(internal)?;
    match write {
        Some(write) => Ok(no_store(
            write_response(&pool, write, StatusCode::OK).await?,
        )),
        None => Ok(no_store(record_not_found(ScheduledTaskModel::not_found()))),
    }
}

async fn console_destroy(
    State(state): State<AppState>,
    user: ConsoleUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let Some(task_id) = ScheduledTaskModel::decode_oid(&id) else {
        return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
    };
    let deleted = db::delete_task(&pool, db::TaskOwner::Author(user.id), task_id)
        .await
        .map_err(internal)?;
    if !deleted {
        return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
    }
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}

async fn console_run(
    State(state): State<AppState>,
    user: ConsoleUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let task = match ScheduledTaskModel::decode_oid(&id) {
        Some(task_id) => db::find_author_task(&pool, user.id, task_id)
            .await
            .map_err(internal)?,
        None => None,
    };
    let Some(task) = task else {
        return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
    };
    if !task.enabled {
        return Ok(no_store(
            ConsoleError::unprocessable("Enable the task before running it.")
                .with_reason("task_disabled")
                .into_response(),
        ));
    }
    match start_run(&state, &pool, task.id, task.author_id).await {
        Ok(run_id) => {
            let payload = db::console_payload_for(&pool, &task)
                .await
                .map_err(internal)?;
            // The manual run adds the queue state to the stored task's payload.
            let mut data = serde_json::to_value(payload).map_err(internal)?;
            data["queued"] = json!(true);
            data["run_id"] = json!(run_id);
            Ok(no_store(
                (StatusCode::ACCEPTED, Json(DataPayload { data })).into_response(),
            ))
        }
        Err(error) => Ok(no_store(run_error(&error))),
    }
}

async fn console_channel_options(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleQuery(query): ConsoleQuery<ChannelOptionsQuery>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    if let Some(task_id) = query.task_id.as_deref().filter(|value| !value.is_empty()) {
        let found = match ScheduledTaskModel::decode_oid(task_id) {
            Some(task_id) => db::find_author_task(&pool, user.id, task_id)
                .await
                .map_err(internal)?,
            None => None,
        };
        if found.is_none() {
            return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
        }
    }
    let policy = db::slack_policy_for_user(&pool, user.id)
        .await
        .map_err(internal)?;
    let options =
        db::slack_channel_options(&pool, &policy, query.q.as_deref(), db::MAX_CHANNEL_OPTIONS)
            .await
            .map_err(internal)?;
    let catalog_error = db::catalog_error(&pool, &policy).await.map_err(internal)?;
    let body = ChannelOptionsPayload {
        data: options,
        meta: ChannelOptionsMeta { catalog_error },
    };
    Ok(no_store(Json(body).into_response()))
}

#[derive(Serialize)]
struct ChannelOptionsPayload {
    data: Vec<db::ChannelOption>,
    meta: ChannelOptionsMeta,
}

/// The console form's catalog notice, beside the options themselves.
#[derive(Serialize)]
struct ChannelOptionsMeta {
    catalog_error: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct ChannelOptionsQuery {
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    q: Option<String>,
}

/// The permitted input: only the fields the form sends are changed, so a
/// partial update keeps the stored schedule and destination.
#[derive(Debug, Default, serde::Deserialize)]
struct ConsoleTaskData {
    #[serde(default, deserialize_with = "nullable_field")]
    name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    prompt: Option<Option<String>>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default, deserialize_with = "nullable_field")]
    delivery_mode: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    delivery_channel: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    schedule_preset: Option<Option<String>>,
    #[serde(default)]
    cron_expression: Option<String>,
    #[serde(default)]
    custom_time: Option<String>,
    #[serde(default)]
    custom_days: Option<Vec<String>>,
}

async fn console_draft(
    pool: &sqlx::PgPool,
    author_id: i64,
    data: &ConsoleTaskData,
) -> Result<db::TaskDraft, ApiError> {
    let mut draft = db::TaskDraft {
        name: data.name.clone().map(|name| name.unwrap_or_default()),
        prompt: data.prompt.clone().map(|prompt| prompt.unwrap_or_default()),
        enabled: data.enabled,
        ..db::TaskDraft::default()
    };
    if let Some(preset) = &data.schedule_preset {
        let preset = preset.clone().unwrap_or_default();
        let days = data.custom_days.clone().unwrap_or_default();
        let cron = db::cron_for(
            &preset,
            data.cron_expression.as_deref(),
            &days,
            data.custom_time.as_deref(),
        );
        draft.cron_expression = Some(match cron {
            Some(expression) if expression.is_empty() && preset == "custom" => {
                Some(db::INVALID_CUSTOM_SCHEDULE.to_owned())
            }
            Some(expression) if expression.is_empty() => None,
            other => other,
        });
    }
    if data.delivery_mode.is_some() || data.delivery_channel.is_some() {
        let mode = data
            .delivery_mode
            .clone()
            .flatten()
            .filter(|mode| !mode.is_empty())
            .unwrap_or_else(|| "channel".to_owned());
        let channel = match mode.as_str() {
            "dm" => {
                db::slack_policy_for_user(pool, author_id)
                    .await
                    .map_err(internal)?
                    .slack_user_id
            }
            "channel" => data.delivery_channel.clone().flatten(),
            _ => None,
        };
        draft.delivery_channel = Some(channel);
    }
    Ok(draft)
}

async fn write_response(
    pool: &sqlx::PgPool,
    write: db::TaskWrite,
    status: StatusCode,
) -> Result<Response, ApiError> {
    let Some(task) = write.task else {
        return Ok(invalid(write.errors));
    };
    let payload = db::console_payload_for(pool, &task)
        .await
        .map_err(internal)?;
    Ok((status, Json(DataPayload { data: payload })).into_response())
}

// ---------------------------------------------------------------------------
// Bearer: /api/v1/scheduled_tasks/:id
// ---------------------------------------------------------------------------

/// The bearer task payload: the execution state a bearer API client reads.
#[derive(Serialize)]
struct BearerTaskPayload<'a> {
    id: String,
    enabled: bool,
    delivery_channel: &'a str,
}

async fn bearer_show(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let task = match ScheduledTaskModel::decode_oid(&id) {
        Some(task_id) => {
            model::find_by_id::<ScheduledTaskModel, db::ScheduledTask, _>(&pool, task_id)
                .await
                .map_err(internal)?
        }
        None => None,
    };
    let Some(task) = task else {
        return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
    };
    let payload = BearerTaskPayload {
        id: ScheduledTaskModel::oid(task.id),
        enabled: task.enabled,
        delivery_channel: task.delivery_channel.as_str(),
    };
    Ok(no_store(
        Json(DataPayload { data: payload }).into_response(),
    ))
}

// ---------------------------------------------------------------------------
// Sandbox: /api/v1/sandbox/scheduled_tasks
// ---------------------------------------------------------------------------

async fn sandbox_index(
    State(state): State<AppState>,
    context: SandboxContext,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (owner, _) = match sandbox_owner(&pool, &context, None, false).await? {
        Ok(owner) => owner,
        Err(response) => return Ok(response),
    };
    let tasks = match owner {
        db::TaskOwner::Author(author_id) => db::list_author_tasks(&pool, author_id).await,
        db::TaskOwner::Channel(principal_id) => db::list_principal_tasks(&pool, principal_id).await,
    }
    .map_err(internal)?;
    Ok(no_store(
        Json(DataPayload {
            data: db::sandbox_payloads(&tasks),
        })
        .into_response(),
    ))
}

async fn sandbox_show(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (owner, _) = match sandbox_owner(&pool, &context, None, false).await? {
        Ok(owner) => owner,
        Err(response) => return Ok(response),
    };
    let Some(task) = sandbox_task(&pool, owner, &id).await? else {
        return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
    };
    Ok(no_store(
        Json(DataPayload {
            data: db::sandbox_payload(&task),
        })
        .into_response(),
    ))
}

async fn sandbox_create(
    State(state): State<AppState>,
    context: SandboxContext,
    ConsoleData(data): ConsoleData<SandboxTaskData>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (owner, personal) = match sandbox_owner(&pool, &context, Some(&data), true).await? {
        Ok(owner) => owner,
        Err(response) => return Ok(response),
    };
    let draft = match sandbox_draft(&pool, owner, personal, &data, true).await? {
        Ok(draft) => draft,
        Err(error) => return Ok(no_store(error.into_response())),
    };
    let write = db::create_task(&pool, owner, draft)
        .await
        .map_err(internal)?;
    Ok(no_store(sandbox_write_response(write, StatusCode::CREATED)))
}

async fn sandbox_update(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
    ConsoleData(data): ConsoleData<SandboxTaskData>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (owner, personal) = match sandbox_owner(&pool, &context, Some(&data), false).await? {
        Ok(owner) => owner,
        Err(response) => return Ok(response),
    };
    let Some(task_id) = ScheduledTaskModel::decode_oid(&id) else {
        return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
    };
    let draft = match sandbox_draft(&pool, owner, personal, &data, false).await? {
        Ok(draft) => draft,
        Err(error) => return Ok(no_store(error.into_response())),
    };
    let write = db::update_task(&pool, owner, task_id, draft)
        .await
        .map_err(internal)?;
    match write {
        Some(write) => Ok(no_store(sandbox_write_response(write, StatusCode::OK))),
        None => Ok(no_store(record_not_found(ScheduledTaskModel::not_found()))),
    }
}

async fn sandbox_destroy(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (owner, _) = match sandbox_owner(&pool, &context, None, false).await? {
        Ok(owner) => owner,
        Err(response) => return Ok(response),
    };
    let Some(task_id) = ScheduledTaskModel::decode_oid(&id) else {
        return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
    };
    let deleted = db::delete_task(&pool, owner, task_id)
        .await
        .map_err(internal)?;
    if !deleted {
        return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
    }
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}

async fn sandbox_run(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (owner, _) = match sandbox_owner(&pool, &context, None, false).await? {
        Ok(owner) => owner,
        Err(response) => return Ok(response),
    };
    let Some(task) = sandbox_task(&pool, owner, &id).await? else {
        return Ok(no_store(record_not_found(ScheduledTaskModel::not_found())));
    };
    if !task.enabled {
        return Ok(no_store(
            ConsoleError::unprocessable("scheduled task is disabled").into_response(),
        ));
    }
    match start_run(&state, &pool, task.id, task.author_id).await {
        Ok(run_id) => {
            let mut data = serde_json::to_value(db::sandbox_payload(&task)).map_err(internal)?;
            data["queued"] = json!(true);
            data["run_id"] = json!(run_id);
            Ok(no_store(
                (StatusCode::ACCEPTED, Json(DataPayload { data })).into_response(),
            ))
        }
        Err(error) => Ok(no_store(run_error(&error))),
    }
}

async fn sandbox_task(
    pool: &sqlx::PgPool,
    owner: db::TaskOwner,
    id: &str,
) -> Result<Option<db::ScheduledTask>, ApiError> {
    let Some(task_id) = ScheduledTaskModel::decode_oid(id) else {
        return Ok(None);
    };
    match owner {
        db::TaskOwner::Author(author_id) => db::find_author_task(pool, author_id, task_id).await,
        db::TaskOwner::Channel(principal_id) => {
            db::find_principal_task(pool, principal_id, task_id).await
        }
    }
    .map_err(internal)
}

/// The principal a sandbox request acts for, and who sent the turn.
async fn sandbox_principal(
    pool: &sqlx::PgPool,
    context: &SandboxContext,
) -> Result<Option<Principal>, ApiError> {
    let Some(principal_id) = context.principal_id() else {
        return Ok(None);
    };
    centaur_console_db::find_principal(pool, principal_id)
        .await
        .map_err(internal)
}

/// The `owner` value that asks for a personal task.
const PERSONAL_OWNER: &str = "me";

/// Whether this request asked for a personal task. Any other owner value, and
/// an owner on anything but create, is rejected before the actor checks.
fn requested_owner(data: Option<&SandboxTaskData>, creating: bool) -> Result<bool, ConsoleError> {
    let Some(data) = data else {
        return Ok(false);
    };
    match &data.owner {
        Value::Null => Ok(false),
        Value::String(owner) if owner == PERSONAL_OWNER => {
            if creating {
                Ok(true)
            } else {
                Err(ConsoleError::unprocessable(
                    "owner can only be set when creating a task",
                ))
            }
        }
        _ => Err(ConsoleError::unprocessable(format!(
            "owner must be \"{PERSONAL_OWNER}\""
        ))),
    }
}

/// The task owner a sandbox request may manage, or the response that refuses
/// it.
async fn sandbox_owner(
    pool: &sqlx::PgPool,
    context: &SandboxContext,
    data: Option<&SandboxTaskData>,
    creating: bool,
) -> Result<Result<(db::TaskOwner, bool), Response>, ApiError> {
    let personal = match requested_owner(data, creating) {
        Ok(personal) => personal,
        Err(error) => return Ok(Err(no_store(error.into_response()))),
    };
    let Some(principal) = sandbox_principal(pool, context).await? else {
        return Ok(Err(sandbox_forbidden()));
    };
    if principal.kind == "slack_channel" {
        if personal {
            return Ok(match personal_channel_author(pool, context).await? {
                Some(author_id) => Ok((db::TaskOwner::Author(author_id), true)),
                None => Err(sandbox_forbidden()),
            });
        }
        if channel_requester(pool, context).await?.is_none() {
            return Ok(Err(sandbox_forbidden()));
        }
        return Ok(Ok((db::TaskOwner::Channel(principal.id), false)));
    }
    let Some(user_id) = principal.console_user_id else {
        return Ok(Err(sandbox_forbidden()));
    };
    let user = centaur_console_db::find_console_user(pool, user_id)
        .await
        .map_err(internal)?;
    Ok(
        match user.filter(|user| user.status == ConsoleUserStatus::Active) {
            Some(user) => Ok((db::TaskOwner::Author(user.id), personal)),
            None => Err(sandbox_forbidden()),
        },
    )
}

/// The Slack user who sent the current turn, when the request may act for a
/// channel. Only a Slack user from a workspace the bot is installed in counts;
/// a Slack Connect guest does not.
async fn channel_requester(
    pool: &sqlx::PgPool,
    context: &SandboxContext,
) -> Result<Option<Principal>, ApiError> {
    // A Code Mode tool-host token has no proxy, so it has no requester.
    let Some(proxy) = &context.proxy else {
        return Ok(None);
    };
    let Some(requester) = db::proxy_requester(pool, proxy.id)
        .await
        .map_err(internal)?
    else {
        return Ok(None);
    };
    if requester.kind != "slack_dm" || requester.slack_user_id.as_deref().is_none_or(str::is_empty)
    {
        return Ok(None);
    }
    let Some(team_id) = requester
        .slack_team_id
        .as_deref()
        .filter(|team| !team.is_empty())
    else {
        return Ok(None);
    };
    let home = centaur_console_db::slack_bot_home_team(pool, team_id)
        .await
        .map_err(internal)?;
    Ok(home.then_some(requester))
}

/// The owner of a personal task comes only from the attested identity of the
/// current turn: the requester's linked Console user, whose own Slack DM must
/// be that requester. A request parameter can never name someone else.
async fn personal_channel_author(
    pool: &sqlx::PgPool,
    context: &SandboxContext,
) -> Result<Option<i64>, ApiError> {
    let Some(requester) = channel_requester(pool, context).await? else {
        return Ok(None);
    };
    let Some(user_id) = requester.console_user_id else {
        return Ok(None);
    };
    let Some(user) = centaur_console_db::find_console_user(pool, user_id)
        .await
        .map_err(internal)?
    else {
        return Ok(None);
    };
    if user.status != ConsoleUserStatus::Active {
        return Ok(None);
    }
    let policy = db::slack_policy_for_user(pool, user.id)
        .await
        .map_err(internal)?;
    let matches = policy.slack_user_id.as_deref().is_some_and(|identity| {
        !identity.is_empty() && requester.slack_user_id.as_deref() == Some(identity)
    });
    Ok(matches.then_some(user.id))
}

/// The one 403 every refused sandbox actor gets.
fn sandbox_forbidden() -> Response {
    no_store(
        ConsoleError::new(
            StatusCode::FORBIDDEN,
            "sandbox request is not allowed to manage scheduled tasks",
        )
        .into_response(),
    )
}

#[derive(Debug, Default, serde::Deserialize)]
struct SandboxTaskData {
    #[serde(default, deserialize_with = "nullable_field")]
    name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    prompt: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    delivery_channel: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    cron_expression: Option<Option<String>>,
    #[serde(default)]
    enabled: Option<bool>,
    /// `data.owner`; `"me"` asks for a personal task. Left as a `Value` so a
    /// non-string owner is a 422, not a typed-request 400.
    #[serde(default)]
    owner: Value,
}

async fn sandbox_draft(
    pool: &sqlx::PgPool,
    owner: db::TaskOwner,
    personal: bool,
    data: &SandboxTaskData,
    creating: bool,
) -> Result<Result<db::TaskDraft, ConsoleError>, ApiError> {
    let mut draft = db::TaskDraft {
        name: data.name.clone().map(|name| name.unwrap_or_default()),
        prompt: data.prompt.clone().map(|prompt| prompt.unwrap_or_default()),
        enabled: data.enabled,
        delivery_channel: data.delivery_channel.clone(),
        cron_expression: data.cron_expression.clone(),
    };
    match (personal, owner) {
        (true, db::TaskOwner::Author(author_id)) => {
            let requested = draft.delivery_channel.clone().flatten().unwrap_or_default();
            let requested = requested.trim();
            let policy = db::slack_policy_for_user(pool, author_id)
                .await
                .map_err(internal)?;
            let dm_user_id = policy.slack_user_id;
            let delivers_to_dm = requested.is_empty()
                || requested.eq_ignore_ascii_case("dm")
                || dm_user_id.as_deref().is_some_and(|identity| {
                    !identity.is_empty() && requested.eq_ignore_ascii_case(identity)
                });
            if !delivers_to_dm {
                return Ok(Err(ConsoleError::unprocessable(
                    "personal tasks always deliver to your DM",
                )));
            }
            draft.delivery_channel = Some(dm_user_id);
        }
        (_, db::TaskOwner::Channel(principal_id)) => {
            let channel: Option<String> =
                sqlx::query_scalar("SELECT slack_channel_id FROM principals WHERE id = $1")
                    .bind(principal_id)
                    .fetch_optional(pool)
                    .await
                    .map_err(internal)?;
            let channel = channel.unwrap_or_default();
            let requested = draft.delivery_channel.clone().flatten().unwrap_or_default();
            let matches = requested.is_empty() || requested.trim().eq_ignore_ascii_case(&channel);
            if !matches {
                return Ok(Err(ConsoleError::unprocessable(
                    "tasks created in a Slack channel thread always deliver to that channel",
                )));
            }
            draft.delivery_channel = Some(Some(channel));
        }
        (_, db::TaskOwner::Author(author_id)) => {
            let requested = draft.delivery_channel.clone().flatten().unwrap_or_default();
            let wants_dm = requested.trim().eq_ignore_ascii_case("dm")
                || (creating && draft.delivery_channel.is_none());
            if wants_dm {
                let policy = db::slack_policy_for_user(pool, author_id)
                    .await
                    .map_err(internal)?;
                draft.delivery_channel = Some(policy.slack_user_id);
            }
        }
    }
    Ok(Ok(draft))
}

fn sandbox_write_response(write: db::TaskWrite, status: StatusCode) -> Response {
    match write.task {
        Some(task) => (
            status,
            Json(DataPayload {
                data: db::sandbox_payload(&task),
            }),
        )
            .into_response(),
        None => invalid(write.errors),
    }
}

// ---------------------------------------------------------------------------
// Console: /api/v1/console/workflows
// ---------------------------------------------------------------------------

const WORKFLOW_UNAVAILABLE_MESSAGE: &str = "Workflow database is unavailable. \
     Console needs the API workflow read-only views to show runs.";

fn workflow_unavailable() -> Response {
    no_store(
        ConsoleError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            WORKFLOW_UNAVAILABLE_MESSAGE,
        )
        .with_reason("workflow_database_unavailable")
        .into_response(),
    )
}

fn workflow_reason(status: StatusCode, message: &str) -> Response {
    no_store(
        ConsoleError::new(status, message)
            .with_reason("workflow_api_error")
            .into_response(),
    )
}

/// The pool holding api-rs's published run view, or a 503.
async fn dashboard_pool(state: &AppState) -> Result<sqlx::PgPool, Response> {
    let Ok(pool) = state.pool() else {
        return Err(workflow_unavailable());
    };
    match db::workflow_database_available(&pool).await {
        Ok(true) => Ok(pool),
        _ => Err(workflow_unavailable()),
    }
}

async fn workflows_index(
    State(state): State<AppState>,
    ConsoleQuery(query): ConsoleQuery<PageQuery>,
) -> Result<Response, Response> {
    // Resolve the page before the database work.
    let page = page_param(query.page.as_deref())?;
    let pool = dashboard_pool(&state).await?;
    let total = db::workflow_count(&pool)
        .await
        .map_err(|error| workflow_load_failed(&error))?;
    let meta = PageMeta::new(total, page);
    let runs = db::latest_per_workflow(
        &pool,
        db::WORKFLOW_PAGE_SIZE,
        (meta.page - 1) * db::WORKFLOW_PAGE_SIZE,
    )
    .await
    .map_err(|error| workflow_load_failed(&error))?;
    let keys: Vec<String> = runs
        .iter()
        .filter_map(db::WorkflowRunRow::workflow_key)
        .collect();
    let breakdown = db::latest_per_queue(&pool, &keys)
        .await
        .map_err(|error| workflow_load_failed(&error))?;
    let rows: Vec<WorkflowRow> = runs
        .iter()
        .map(|run| workflow_row(run, breakdown.get(&run.workflow_key().unwrap_or_default())))
        .collect();
    Ok(no_store(
        Json(json!({ "data": rows, "meta": meta })).into_response(),
    ))
}

async fn workflows_show(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    let pool = dashboard_pool(&state).await?;
    let mut latest = db::workflow_runs(&pool, &id, 1, 0, None, None)
        .await
        .map_err(|error| workflow_load_failed(&error))?;
    let Some(latest_run) = latest.pop() else {
        return Ok(no_store(
            ConsoleError::new(
                StatusCode::NOT_FOUND,
                format!("No workflow runs found for {id}."),
            )
            .into_response(),
        ));
    };
    let status_counts = db::workflow_status_counts(&pool, &id)
        .await
        .map_err(|error| workflow_load_failed(&error))?;
    let queue_names = db::workflow_queue_names(&pool, &id)
        .await
        .map_err(|error| workflow_load_failed(&error))?;
    let schedule = schedule_for(&state, &id).await;
    let debug = debug_rows(
        &state,
        &pool,
        &id,
        &latest_run,
        &status_counts,
        schedule.as_ref(),
    )
    .await;
    let payload = overview(
        &id,
        &latest_run,
        &status_counts,
        &queue_names,
        schedule.as_ref(),
        debug,
    );
    Ok(no_store(
        Json(DataPayload { data: payload }).into_response(),
    ))
}

async fn workflows_runs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ConsoleQuery(query): ConsoleQuery<RunsQuery>,
) -> Result<Response, Response> {
    let page = page_param(query.page.as_deref())?;
    let pool = dashboard_pool(&state).await?;
    let status = query.status.as_deref().filter(|value| !value.is_empty());
    let queue = query.queue.as_deref().filter(|value| !value.is_empty());
    let total = db::workflow_run_count(&pool, &id, status, queue)
        .await
        .map_err(|error| workflow_load_failed(&error))?;
    let meta = PageMeta::new(total, page);
    let runs = db::workflow_runs(
        &pool,
        &id,
        db::WORKFLOW_PAGE_SIZE,
        (meta.page - 1) * db::WORKFLOW_PAGE_SIZE,
        status,
        queue,
    )
    .await
    .map_err(|error| workflow_load_failed(&error))?;
    let rows: Vec<RunRowPayload> = runs.iter().map(RunRowPayload::new).collect();
    Ok(no_store(
        Json(json!({ "data": rows, "meta": meta })).into_response(),
    ))
}

async fn workflows_create_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, Response> {
    // The workflow runtime is in-process in api-rs; a missing runtime is the
    // workflow control plane being unavailable, so it keeps the API error
    // envelope.
    let workflows = state
        .workflows()
        .map_err(|error| workflow_reason(StatusCode::BAD_GATEWAY, &error.to_string()))?;
    let schedule = schedule_for(&state, &id).await;
    let input = schedule
        .as_ref()
        .and_then(|schedule| schedule.get("input"))
        .cloned();
    let response = workflows
        .create_run(CreateWorkflowRunRequest {
            workflow_name: id.clone(),
            input: input.unwrap_or(Value::Null),
            idempotency_key: None,
            harness_type: None,
            max_attempts: None,
        })
        .await
        .map_err(|error| workflow_reason(StatusCode::BAD_GATEWAY, &error.to_string()))?;
    let status = if response.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let data = RunCreatedPayload {
        run_id: response.run_id,
        created: response.created,
    };
    Ok(no_store(
        (status, Json(DataPayload { data })).into_response(),
    ))
}

#[derive(Debug, Default, serde::Deserialize)]
struct PageQuery {
    #[serde(default)]
    page: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RunsQuery {
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    queue: Option<String>,
}

/// A non-integer page keeps Console's 400.
fn page_param(raw: Option<&str>) -> Result<i64, Response> {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Ok(1);
    };
    match raw.trim().parse::<i64>() {
        Ok(page) => Ok(page.max(1)),
        Err(_) => Err(no_store(
            ConsoleError::bad_request("page must be an integer").into_response(),
        )),
    }
}

/// The dashboard's reads render `workflow_database_unavailable`;
/// the read error itself is only visible in the pod log.
fn workflow_load_failed(error: &impl std::fmt::Display) -> Response {
    tracing::warn!(%error, "console workflow dashboard read failed");
    workflow_unavailable()
}

/// The dashboard pages in whole workflow or run counts, so `total_pages` is at
/// least 1 and `page` is clamped into it, unlike the Console index pagination in
/// `console::http`.
#[derive(Serialize)]
struct PageMeta {
    page: i64,
    limit: i64,
    total: i64,
    total_pages: i64,
}

impl PageMeta {
    fn new(total: i64, page: i64) -> Self {
        let total_pages = ((total as f64 / db::WORKFLOW_PAGE_SIZE as f64).ceil() as i64).max(1);
        Self {
            page: page.clamp(1, total_pages),
            limit: db::WORKFLOW_PAGE_SIZE,
            total,
            total_pages,
        }
    }
}

#[derive(Serialize)]
struct RunCreatedPayload {
    run_id: String,
    created: bool,
}

fn duration_label(run: &db::WorkflowRunRow) -> String {
    let started_at = run.started_or_created_at();
    let finished_at = run.terminal_at();
    if started_at.is_some() && finished_at.is_none() && run.display_status() == "running" {
        return "running".to_owned();
    }
    let (Some(started_at), Some(finished_at)) = (started_at, finished_at) else {
        return "—".to_owned();
    };
    distance_of_time_in_words(started_at, finished_at)
}

/// The human-readable duration, for the run durations the console shows.
fn distance_of_time_in_words(from: &str, to: &str) -> String {
    let parse = |value: &str| {
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
    };
    let (Ok(from), Ok(to)) = (parse(from), parse(to)) else {
        return "—".to_owned();
    };
    let seconds = (to - from).whole_seconds().max(0);
    let (count, unit) = match seconds {
        0..=44 => return "less than a minute".to_owned(),
        45..=89 => (1, "minute"),
        90..=2699 => (((seconds as f64) / 60.0).round() as i64, "minute"),
        2700..=5399 => (1, "hour"),
        5400..=86399 => (((seconds as f64) / 3600.0).round() as i64, "hour"),
        86400..=172_799 => (1, "day"),
        _ => (((seconds as f64) / 86400.0).round() as i64, "day"),
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural}")
}

/// The fields every run row carries.
#[derive(Serialize)]
struct RunTiming<'a> {
    status: String,
    queue_name: Option<&'a str>,
    queue_label: String,
    attempts: i32,
    max_attempts: Option<i32>,
    started_at: Option<&'a str>,
    created_at: Option<&'a str>,
    finished_at: Option<&'a str>,
}

impl<'a> RunTiming<'a> {
    fn new(run: &'a db::WorkflowRunRow) -> Self {
        Self {
            status: run.display_status(),
            queue_name: run.queue_name.as_deref(),
            queue_label: run.queue_label(),
            attempts: run.attempts.unwrap_or_default(),
            max_attempts: run.max_attempts,
            started_at: run.started_or_created_at(),
            created_at: run.created_at.as_deref(),
            finished_at: run.terminal_at(),
        }
    }
}

#[derive(Serialize)]
struct RunRowPayload<'a> {
    run_id: Option<&'a str>,
    task_id: Option<&'a str>,
    #[serde(flatten)]
    timing: RunTiming<'a>,
}

impl<'a> RunRowPayload<'a> {
    fn new(run: &'a db::WorkflowRunRow) -> Self {
        Self {
            run_id: run.run_id.as_deref(),
            task_id: run.task_id.as_deref(),
            timing: RunTiming::new(run),
        }
    }
}

/// The latest run of a workflow with each of its queues.
#[derive(Serialize)]
struct WorkflowRow<'a> {
    workflow_key: Option<String>,
    label: String,
    task_name: Option<&'a str>,
    #[serde(flatten)]
    timing: RunTiming<'a>,
    queues: Vec<WorkflowQueueRow<'a>>,
}

/// One queue of [`WorkflowRow`].
#[derive(Serialize)]
struct WorkflowQueueRow<'a> {
    queue_name: Option<&'a str>,
    queue_label: String,
    status: String,
    last_activity_at: Option<&'a str>,
    run_count: i64,
}

impl<'a> WorkflowQueueRow<'a> {
    fn new(run: &'a db::WorkflowRunRow) -> Self {
        Self {
            queue_name: run.queue_name.as_deref(),
            queue_label: run.queue_label(),
            status: run.display_status(),
            last_activity_at: run.recency_at(),
            run_count: run.queue_run_count.unwrap_or_default(),
        }
    }
}

fn workflow_row<'a>(
    run: &'a db::WorkflowRunRow,
    queue_runs: Option<&'a Vec<db::WorkflowRunRow>>,
) -> WorkflowRow<'a> {
    WorkflowRow {
        workflow_key: run.workflow_key(),
        label: run.workflow_name_label(),
        task_name: run.task_name.as_deref().filter(|name| !name.is_empty()),
        timing: RunTiming::new(run),
        queues: queue_runs
            .map(|queue_runs| queue_runs.iter().map(WorkflowQueueRow::new).collect())
            .unwrap_or_default(),
    }
}

#[derive(Serialize)]
struct OverviewPayload<'a> {
    workflow_key: &'a str,
    label: String,
    task_name: Option<&'a str>,
    #[serde(flatten)]
    timing: RunTiming<'a>,
    harness_type: Option<&'a str>,
    engine_label: Option<String>,
    duration_label: String,
    run_count: i64,
    status_counts: Vec<StatusCount>,
    queues: Vec<QueueSummary<'a>>,
    schedule: Option<SchedulePayload>,
    debug: Vec<DebugRow>,
}

/// One registered queue of a workflow.
#[derive(Serialize)]
struct QueueSummary<'a> {
    queue_name: &'a str,
    queue_label: String,
}

/// One entry of the overview's `status_counts`.
#[derive(Serialize)]
struct StatusCount {
    status: String,
    count: i64,
}

fn overview<'a>(
    workflow_name: &'a str,
    latest_run: &'a db::WorkflowRunRow,
    status_counts: &std::collections::BTreeMap<String, i64>,
    queue_names: &'a [String],
    schedule: Option<&Value>,
    debug: Vec<DebugRow>,
) -> OverviewPayload<'a> {
    OverviewPayload {
        workflow_key: workflow_name,
        label: latest_run.workflow_name_label(),
        task_name: latest_run
            .task_name
            .as_deref()
            .filter(|name| !name.is_empty()),
        timing: RunTiming::new(latest_run),
        harness_type: latest_run
            .harness_type
            .as_deref()
            .filter(|value| !value.is_empty()),
        engine_label: latest_run.engine_label(),
        duration_label: duration_label(latest_run),
        run_count: status_counts.values().sum::<i64>(),
        status_counts: ordered_status_counts(status_counts),
        queues: queue_names
            .iter()
            .map(|name| QueueSummary {
                queue_name: name,
                queue_label: db::queue_label(Some(name)),
            })
            .collect(),
        schedule: schedule_payload(schedule),
        debug,
    }
}

fn ordered_status_counts(
    status_counts: &std::collections::BTreeMap<String, i64>,
) -> Vec<StatusCount> {
    const STATUS_ORDER: [&str; 6] = [
        "running",
        "pending",
        "sleeping",
        "completed",
        "failed",
        "cancelled",
    ];
    let mut keys: Vec<String> = STATUS_ORDER
        .iter()
        .filter(|status| status_counts.contains_key(**status))
        .map(|status| (*status).to_owned())
        .collect();
    let mut remaining: Vec<String> = status_counts
        .keys()
        .filter(|status| !STATUS_ORDER.contains(&status.as_str()))
        .cloned()
        .collect();
    remaining.sort();
    keys.extend(remaining);
    keys.into_iter()
        .map(|status| StatusCount {
            count: status_counts[&status],
            status,
        })
        .collect()
}

/// The registered schedule of a workflow, as the dashboard renders it.
#[derive(Serialize)]
struct SchedulePayload {
    label: Option<String>,
    summary: Option<String>,
    timezone: Option<String>,
    enabled: bool,
    source_path: Option<String>,
    source_url: Option<String>,
}

/// The registered schedule without its input.
fn schedule_payload(schedule: Option<&Value>) -> Option<SchedulePayload> {
    let schedule = schedule?;
    let kind = schedule.get("kind").filter(|kind| kind.is_object());
    let timezone = schedule
        .get("timezone")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let label = db::workflow_schedule_label(schedule);
    let enabled = schedule.get("enabled") != Some(&Value::Bool(false));
    let summary: Vec<String> = [
        label.clone(),
        (kind
            .and_then(|kind| kind.get("type"))
            .and_then(Value::as_str)
            == Some("cron"))
        .then(|| timezone.map(str::to_owned))
        .flatten(),
        (!enabled).then(|| "disabled".to_owned()),
    ]
    .into_iter()
    .flatten()
    .collect();
    let source_path = schedule
        .get("source_path")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    Some(SchedulePayload {
        summary: (!summary.is_empty()).then(|| summary.join(" · ")),
        timezone: timezone.map(str::to_owned),
        enabled,
        source_path: source_path.map(str::to_owned),
        source_url: db::source_url(source_path),
        label,
    })
}

/// The registered schedule of one workflow,
/// `None` when it has none or the workflow runtime is unreachable.
async fn schedule_for(state: &AppState, workflow_name: &str) -> Option<Value> {
    let workflows = state.workflows().ok()?;
    workflows
        .list_schedules()
        .into_iter()
        .find(|schedule| schedule.workflow_name == workflow_name)
        .and_then(|schedule| serde_json::to_value(schedule).ok())
}

async fn fetch_run_detail(state: &AppState, run_id: Option<&str>) -> Option<Value> {
    let run_id = run_id.filter(|run_id| !run_id.is_empty())?;
    let workflows = state.workflows().ok()?;
    let run = workflows.get_run(run_id).await.ok()?;
    serde_json::to_value(run).ok()
}

/// One row of the dashboard's `debug` block.
#[derive(Serialize)]
struct DebugRow {
    label: &'static str,
    value: Value,
    run_id: Option<String>,
}

async fn debug_rows(
    state: &AppState,
    pool: &sqlx::PgPool,
    workflow_name: &str,
    latest_run: &db::WorkflowRunRow,
    status_counts: &std::collections::BTreeMap<String, i64>,
    schedule: Option<&Value>,
) -> Vec<DebugRow> {
    let mut rows = Vec::new();
    let detail = fetch_run_detail(state, latest_run.run_id.as_deref()).await;
    if let Some(detail) = detail.as_ref() {
        let input = detail.get("input").cloned().unwrap_or(Value::Null);
        let empty = input.is_null() || input == json!({}) || input == Value::String(String::new());
        if !empty {
            rows.push(debug_row("Input", input, None));
        }
        if let Some(result) = detail.get("result").filter(|value| !value.is_null()) {
            rows.push(debug_row("Result", result.clone(), None));
        }
        if let Some(failure) = detail.get("failure").filter(|value| !value.is_null()) {
            let run_id = detail.get("run_id").and_then(Value::as_str);
            rows.push(debug_row("Failure", failure.clone(), run_id));
        }
    }
    if let Some(failure) = latest_failure_detail(
        state,
        pool,
        workflow_name,
        latest_run,
        status_counts,
        detail.as_ref(),
        schedule,
    )
    .await
        && let Some(value) = failure.get("failure").filter(|value| !value.is_null())
    {
        let run_id = failure.get("run_id").and_then(Value::as_str);
        rows.push(debug_row("Last failure", value.clone(), run_id));
    }
    rows
}

async fn latest_failure_detail(
    state: &AppState,
    pool: &sqlx::PgPool,
    workflow_name: &str,
    latest_run: &db::WorkflowRunRow,
    status_counts: &std::collections::BTreeMap<String, i64>,
    detail: Option<&Value>,
    schedule: Option<&Value>,
) -> Option<Value> {
    if detail.is_none() && schedule.is_none() {
        return None;
    }
    if latest_run.display_status() == "failed" {
        return None;
    }
    if status_counts.get("failed").copied().unwrap_or_default() < 1 {
        return None;
    }
    let failed = db::workflow_runs(pool, workflow_name, 1, 0, Some("failed"), None)
        .await
        .ok()?;
    fetch_run_detail(state, failed.first().and_then(|run| run.run_id.as_deref())).await
}

fn debug_row(label: &'static str, value: Value, run_id: Option<&str>) -> DebugRow {
    DebugRow {
        label,
        value,
        run_id: run_id.map(str::to_owned),
    }
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

/// Why a manual run did not start.
enum RunFailure {
    /// There is no reachable control plane to start it.
    Unavailable,
    /// The control plane refused the run.
    Refused(StartError),
}

/// Provision the author's principal, then start the
/// run in-process. A workflow runtime that is not enabled is an unreachable
/// control plane.
async fn start_run(
    state: &AppState,
    pool: &sqlx::PgPool,
    task_id: i64,
    author_id: Option<i64>,
) -> Result<String, RunFailure> {
    if let Some(author_id) = author_id {
        db::provision_author_principal(pool, author_id)
            .await
            .map_err(|error| {
                RunFailure::Refused(StartError {
                    retryable: true,
                    message: error.to_string(),
                })
            })?;
    }
    // An unreachable control plane surfaces as an unhandled error. In production
    // api-rs serves the route itself and the in-process runtime is always there.
    let workflows = match state.workflows() {
        Ok(workflows) => workflows,
        Err(_) => return Err(RunFailure::Unavailable),
    };
    let start = |request: RunRequest| {
        let workflows = workflows.clone();
        async move {
            workflows
                .create_run(CreateWorkflowRunRequest {
                    workflow_name: request.workflow_name.to_owned(),
                    input: request.input,
                    idempotency_key: Some(request.idempotency_key),
                    harness_type: None,
                    max_attempts: Some(request.max_attempts),
                })
                .await
                .map(|run| run.run_id)
                .map_err(|error| StartError {
                    retryable: !matches!(
                        error,
                        centaur_workflows::WorkflowRuntimeError::BadRequest(_)
                            | centaur_workflows::WorkflowRuntimeError::Disabled(_)
                            | centaur_workflows::WorkflowRuntimeError::NotFound(_)
                    ),
                    message: error.to_string(),
                })
        }
    };
    match scheduled_tasks::run_now(pool, task_id, None, Utc::now(), start).await {
        Ok(ManualRun::Started { run_id }) => Ok(run_id),
        Ok(ManualRun::NotFound) => Err(RunFailure::Refused(StartError {
            retryable: false,
            message: "scheduled task not found".to_owned(),
        })),
        Ok(ManualRun::Disabled) => Err(RunFailure::Refused(StartError {
            retryable: false,
            message: "scheduled task is disabled".to_owned(),
        })),
        Ok(ManualRun::Failed(error)) => Err(RunFailure::Refused(error)),
        Err(error) => Err(RunFailure::Refused(StartError {
            retryable: true,
            message: error.to_string(),
        })),
    }
}

/// A 422 explains itself, anything
/// else means the run could not be started now. A missing control plane is a
/// 500 in the normal error envelope.
fn run_error(error: &RunFailure) -> Response {
    match error {
        RunFailure::Unavailable => ConsoleError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "scheduled task run could not be started",
        )
        .into_response(),
        RunFailure::Refused(error) if error.retryable => ConsoleError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "scheduled task run could not be started",
        )
        .into_response(),
        RunFailure::Refused(error) => {
            ConsoleError::unprocessable(error.message.clone()).into_response()
        }
    }
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    ApiError::Internal(error.to_string())
}

// ---------------------------------------------------------------------------
// Console: /api/v1/console/skills
// ---------------------------------------------------------------------------

#[derive(Debug, Default, serde::Deserialize)]
struct SkillsQuery {
    #[serde(default)]
    tab: Option<String>,
    #[serde(default)]
    q: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct SandboxSkillIndexQuery {
    /// Parsed by `sandbox_skill_limit`, which keeps Console's 400 message.
    #[serde(default)]
    limit: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct SandboxSkillSearchQuery {
    /// Parsed by `sandbox_skill_limit`, which keeps Console's 400 message.
    #[serde(default)]
    limit: Option<String>,
    /// A required search term.
    q: String,
}

/// The console's shape for a user reference.
#[derive(Serialize)]
struct UserPayload<'a> {
    id: String,
    name: Option<&'a str>,
    email: &'a str,
}

impl<'a> UserPayload<'a> {
    fn new(user: &'a db::SkillUser) -> Self {
        Self {
            id: user.oid(),
            name: user.name.as_deref().filter(|name| !name.is_empty()),
            email: user.email.as_str(),
        }
    }

    /// `user_payload(skill.user)` without loading the author again.
    fn author(skill: &'a db::Skill) -> Self {
        Self {
            id: UserModel::oid(skill.user_id),
            name: skill.author_name.as_deref().filter(|name| !name.is_empty()),
            email: skill.author_email.as_str(),
        }
    }
}

/// The summary's `viewer` flags.
#[derive(Serialize)]
struct SkillSummaryViewer {
    owner: bool,
}

/// The console's `skill_summary`.
#[derive(Serialize)]
struct SkillSummaryPayload<'a> {
    id: String,
    name: &'a str,
    description: &'a str,
    visibility: &'a str,
    author: String,
    owner: UserPayload<'a>,
    updated_at: &'a str,
    viewer: SkillSummaryViewer,
}

impl<'a> SkillSummaryPayload<'a> {
    fn new(skill: &'a db::Skill, viewer_id: i64) -> Self {
        Self {
            id: skill.oid(),
            name: skill.name.as_str(),
            description: skill.description.as_str(),
            visibility: skill.visibility.as_str(),
            author: skill.author_label(),
            owner: UserPayload::author(skill),
            updated_at: skill.updated_at.as_str(),
            viewer: SkillSummaryViewer {
                owner: skill.user_id == viewer_id,
            },
        }
    }
}

/// The detail view's `viewer` flags, which replace the summary's.
#[derive(Serialize)]
struct SkillDetailViewer {
    owner: bool,
    can_edit: bool,
    can_manage: bool,
    can_unshare: bool,
}

/// The console skill detail: the summary plus the document and its flags.
#[derive(Serialize)]
struct SkillDetailPayload<'a> {
    #[serde(flatten)]
    summary: SkillSummaryPayload<'a>,
    instructions: &'a str,
    lock_version: i32,
    checksum: String,
    created_at: &'a str,
    editors: Vec<UserPayload<'a>>,
    viewer: SkillDetailViewer,
}

fn skill_detail<'a>(
    skill: &'a db::Skill,
    viewer_id: i64,
    viewer_is_admin: bool,
    editors: &'a [db::SkillUser],
) -> SkillDetailPayload<'a> {
    let owner = skill.user_id == viewer_id;
    SkillDetailPayload {
        summary: SkillSummaryPayload::new(skill, viewer_id),
        instructions: skill.content.as_str(),
        lock_version: skill.lock_version,
        checksum: db::skill_checksum(&skill.name, &skill.description, &skill.content),
        created_at: skill.created_at.as_str(),
        editors: editors.iter().map(UserPayload::new).collect(),
        viewer: SkillDetailViewer {
            owner,
            can_edit: owner || editors.iter().any(|editor| editor.id == viewer_id),
            can_manage: owner,
            can_unshare: skill.shared() && (owner || viewer_is_admin),
        },
    }
}

/// The writable skill fields (`name`, `description`, `instructions`,
/// `lock_version`) plus the sandbox create action's `visibility` and the
/// owner's `editor_ids`. Each handler reads only the fields it permits.
#[derive(Debug, Default, serde::Deserialize)]
struct SkillData {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    /// Absent keeps the current optimistic lock check.
    #[serde(default, deserialize_with = "nullable_field")]
    lock_version: Option<Option<i32>>,
    #[serde(default)]
    visibility: Option<String>,
    /// Absent keeps the current editors.
    #[serde(default)]
    editor_ids: Option<Vec<String>>,
}

/// A skill write's `data` attributes. Only the sandbox's create action permits
/// `visibility`; every other action leaves it alone.
fn skill_draft(data: &SkillData, lock_version: bool, visibility: bool) -> db::SkillDraft {
    let mut draft = db::SkillDraft {
        name: data.name.clone(),
        description: data.description.clone(),
        content: data.instructions.clone(),
        visibility: visibility.then(|| data.visibility.clone()).flatten(),
        ..db::SkillDraft::default()
    };
    if lock_version && let Some(version) = data.lock_version {
        draft.lock_version = version;
        draft.lock_version_supplied = true;
    }
    draft
}

/// Content errors reported as `instructions`.
fn skill_validation_response(mut errors: FieldErrors) -> Response {
    errors.rename("content", "instructions");
    ConsoleError::from(errors).into_response()
}

fn skill_write_error(error: db::SkillWriteError) -> Response {
    match error {
        db::SkillWriteError::Invalid(errors) => skill_validation_response(errors),
        db::SkillWriteError::Stale => {
            ConsoleError::new(StatusCode::CONFLICT, db::SKILL_STALE_MESSAGE)
                .with_details(json!({ "reason": "stale" }))
                .into_response()
        }
        db::SkillWriteError::Missing => record_not_found(SkillModel::not_found()),
        db::SkillWriteError::Conflicts => ConsoleError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "record conflicts with an existing record",
        )
        .into_response(),
        db::SkillWriteError::Error(error) => internal(error).into_response(),
    }
}

/// Sandbox skill write errors: unlike the console handler, do not remap content
/// errors or attach a stale reason.
fn sandbox_skill_write_error(error: db::SkillWriteError) -> Response {
    match error {
        db::SkillWriteError::Invalid(errors) => ConsoleError::from(errors).into_response(),
        db::SkillWriteError::Stale => {
            ConsoleError::new(StatusCode::CONFLICT, "record changed since it was loaded")
                .into_response()
        }
        db::SkillWriteError::Missing => record_not_found(SkillModel::not_found()),
        db::SkillWriteError::Conflicts => ConsoleError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "record conflicts with an existing record",
        )
        .into_response(),
        db::SkillWriteError::Error(error) => internal(error).into_response(),
    }
}

async fn skill_detail_response(
    pool: &sqlx::PgPool,
    skill: &db::Skill,
    user: &ConsoleUser,
    status: StatusCode,
) -> Result<Response, ApiError> {
    let editors = db::skill_editors(pool, skill.id).await.map_err(internal)?;
    let payload = skill_detail(skill, user.id, user.admin, &editors);
    Ok((status, Json(DataPayload { data: payload })).into_response())
}

async fn skills_index(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleQuery(query): ConsoleQuery<SkillsQuery>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let tab = if query.tab.as_deref() == Some("mine") {
        db::SkillsTab::Mine
    } else {
        db::SkillsTab::Directory
    };
    let search = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let skills = db::list_skills(&pool, Some(user.id), tab, search)
        .await
        .map_err(internal)?;
    let data: Vec<SkillSummaryPayload> = skills
        .iter()
        .map(|skill| SkillSummaryPayload::new(skill, user.id))
        .collect();
    Ok(Json(DataPayload { data }).into_response())
}

async fn skills_show(
    State(state): State<AppState>,
    user: ConsoleUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let skill = db::catalog_skill(&pool, Some(user.id), &id)
        .await
        .map_err(internal)?;
    let Some(skill) = skill else {
        return Ok(record_not_found(SkillModel::not_found()));
    };
    skill_detail_response(&pool, &skill, &user, StatusCode::OK).await
}

async fn skills_create(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleData(data): ConsoleData<SkillData>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let draft = skill_draft(&data, false, false);
    let editor_oids = data.editor_ids.clone().unwrap_or_default();
    match db::create_skill(&pool, user.id, draft, &editor_oids).await {
        Ok(skill) => skill_detail_response(&pool, &skill, &user, StatusCode::CREATED).await,
        Err(error) => Ok(skill_write_error(error)),
    }
}

async fn skills_update(
    State(state): State<AppState>,
    user: ConsoleUser,
    Path(id): Path<String>,
    ConsoleData(data): ConsoleData<SkillData>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let skill = db::editable_skill(&pool, user.id, &id)
        .await
        .map_err(internal)?;
    let Some(skill) = skill else {
        return Ok(record_not_found(SkillModel::not_found()));
    };
    let draft = skill_draft(&data, true, false);
    // Only the owner manages editors; an editor's write keeps them.
    let editor_oids = if skill.user_id == user.id {
        data.editor_ids.clone()
    } else {
        None
    };
    match db::update_skill(&pool, &skill, draft, editor_oids.as_deref()).await {
        Ok(skill) => skill_detail_response(&pool, &skill, &user, StatusCode::OK).await,
        Err(error) => Ok(skill_write_error(error)),
    }
}

async fn skills_destroy(
    State(state): State<AppState>,
    user: ConsoleUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let skill = db::owned_skill(&pool, user.id, &id)
        .await
        .map_err(internal)?;
    let Some(skill) = skill else {
        return Ok(record_not_found(SkillModel::not_found()));
    };
    db::archive_skill(&pool, skill.id).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// The four share and unshare routes: change the visibility, then reload the
/// skill the write touched.
async fn set_shared(
    pool: &sqlx::PgPool,
    skill: &db::Skill,
    shared: bool,
) -> Result<Option<db::Skill>, ApiError> {
    if shared {
        db::share_skill(pool, skill.id).await
    } else {
        db::unshare_skill(pool, skill.id).await
    }
    .map_err(internal)?;
    db::skill_by_id(pool, skill.id).await.map_err(internal)
}

/// [`set_shared`] rendered as the console's skill detail.
async fn console_shared(
    pool: &sqlx::PgPool,
    skill: &db::Skill,
    user: &ConsoleUser,
    shared: bool,
) -> Result<Response, ApiError> {
    let Some(skill) = set_shared(pool, skill, shared).await? else {
        return Ok(record_not_found(SkillModel::not_found()));
    };
    skill_detail_response(pool, &skill, user, StatusCode::OK).await
}

async fn skills_share(
    State(state): State<AppState>,
    user: ConsoleUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let skill = db::owned_skill(&pool, user.id, &id)
        .await
        .map_err(internal)?;
    let Some(skill) = skill else {
        return Ok(record_not_found(SkillModel::not_found()));
    };
    console_shared(&pool, &skill, &user, true).await
}

async fn skills_unshare(
    State(state): State<AppState>,
    user: ConsoleUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    // An admin may also unshare any shared skill (moderation).
    let skill = if user.admin {
        db::admin_unshareable_skill(&pool, user.id, &id)
            .await
            .map_err(internal)?
    } else {
        db::owned_skill(&pool, user.id, &id)
            .await
            .map_err(internal)?
    };
    let Some(skill) = skill else {
        return Ok(record_not_found(SkillModel::not_found()));
    };
    console_shared(&pool, &skill, &user, false).await
}

async fn skill_editor_candidates(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleQuery(query): ConsoleQuery<SkillsQuery>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let candidates = db::skill_editor_candidates(&pool, user.id, query.q.as_deref())
        .await
        .map_err(internal)?;
    Ok(Json(DataPayload {
        data: candidates.iter().map(UserPayload::new).collect::<Vec<_>>(),
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Sandbox: /api/v1/sandbox/skills
// ---------------------------------------------------------------------------

const SANDBOX_SKILL_MAX_LIMIT: i64 = 20;

/// An integer clamped to 1..=20, or a 400. The raw value stays a
/// string so the parser keeps Console's message and clamping, like
/// `Pagination`.
fn sandbox_skill_limit(limit: Option<&str>) -> Result<i64, ConsoleError> {
    let raw = limit.unwrap_or("10");
    match raw.parse::<i64>() {
        Ok(value) => Ok(value.clamp(1, SANDBOX_SKILL_MAX_LIMIT)),
        Err(_) => Err(ConsoleError::bad_request("limit must be an integer")),
    }
}

/// The request's principal, when it links an active
/// console user.
async fn sandbox_linked_user(
    pool: &sqlx::PgPool,
    context: &SandboxContext,
) -> Result<Option<ConsoleUser>, ApiError> {
    let Some(principal) = sandbox_principal(pool, context).await? else {
        return Ok(None);
    };
    let Some(user_id) = principal.console_user_id else {
        return Ok(None);
    };
    let user = centaur_console_db::find_console_user(pool, user_id)
        .await
        .map_err(internal)?;
    Ok(user.filter(|user| user.status == ConsoleUserStatus::Active))
}

fn sandbox_authoring_forbidden() -> Response {
    ConsoleError::new(
        StatusCode::FORBIDDEN,
        "sandbox principal is not linked to an active Console user",
    )
    .into_response()
}

/// The authoring user and the skill they may write, or the
/// response that refuses the request.
async fn sandbox_authored_skill(
    pool: &sqlx::PgPool,
    context: &SandboxContext,
    id: &str,
    editors: bool,
) -> Result<Result<(ConsoleUser, db::Skill), Response>, ApiError> {
    let Some(user) = sandbox_linked_user(pool, context).await? else {
        return Ok(Err(sandbox_authoring_forbidden()));
    };
    let skill = if editors {
        db::editable_skill(pool, user.id, id).await
    } else {
        db::owned_skill(pool, user.id, id).await
    }
    .map_err(internal)?;
    Ok(match skill {
        Some(skill) => Ok((user, skill)),
        None => Err(record_not_found(SkillModel::not_found())),
    })
}

/// [`set_shared`] rendered as the sandbox's author payload.
async fn sandbox_shared(
    pool: &sqlx::PgPool,
    skill: &db::Skill,
    shared: bool,
) -> Result<Response, ApiError> {
    let Some(skill) = set_shared(pool, skill, shared).await? else {
        return Ok(record_not_found(SkillModel::not_found()));
    };
    Ok(json_data(sandbox_author_payload(&skill)))
}

#[derive(Serialize)]
struct CatalogPayload<'a> {
    id: String,
    name: &'a str,
    description: &'a str,
    visibility: &'a str,
    author: String,
    updated_at: &'a str,
    checksum: String,
    /// The rendered SKILL.md, only when the action asks for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    document: Option<String>,
}

impl<'a> CatalogPayload<'a> {
    fn new(skill: &'a db::Skill, include_document: bool) -> Self {
        Self {
            id: skill.oid(),
            name: skill.name.as_str(),
            description: skill.description.as_str(),
            visibility: skill.visibility.as_str(),
            author: skill.author_label(),
            updated_at: skill.updated_at.as_str(),
            checksum: db::skill_checksum(&skill.name, &skill.description, &skill.content),
            document: include_document
                .then(|| db::skill_document(&skill.name, &skill.description, &skill.content)),
        }
    }
}

/// The catalog payload plus the optimistic lock version.
#[derive(Serialize)]
struct SandboxAuthorPayload<'a> {
    #[serde(flatten)]
    catalog: CatalogPayload<'a>,
    lock_version: i32,
}

fn sandbox_author_payload(skill: &db::Skill) -> SandboxAuthorPayload<'_> {
    SandboxAuthorPayload {
        catalog: CatalogPayload::new(skill, true),
        lock_version: skill.lock_version,
    }
}

/// One editor in `editor_management_payload`.
#[derive(Serialize)]
struct EditorPayload {
    id: String,
    email: String,
    name: Option<String>,
    status: String,
}

/// `editor_management_payload`.
#[derive(Serialize)]
struct EditorManagementPayload {
    id: String,
    editors: Vec<EditorPayload>,
    lock_version: i32,
}

async fn sandbox_editor_payload(
    pool: &sqlx::PgPool,
    skill: &db::Skill,
) -> Result<EditorManagementPayload, ApiError> {
    let editors = db::skill_editors(pool, skill.id).await.map_err(internal)?;
    Ok(EditorManagementPayload {
        id: skill.oid(),
        editors: editors
            .into_iter()
            .map(|editor| EditorPayload {
                id: editor.oid(),
                email: editor.email,
                name: editor.name,
                status: editor.status,
            })
            .collect(),
        lock_version: skill.lock_version,
    })
}

async fn sandbox_visible_skill(
    pool: &sqlx::PgPool,
    viewer_id: Option<i64>,
    reference: &str,
) -> Result<Result<db::Skill, Response>, ApiError> {
    let skill = if reference.starts_with(&format!("{}_", SkillModel::OID_PREFIX)) {
        db::catalog_skill(pool, viewer_id, reference)
            .await
            .map_err(internal)?
    } else {
        db::catalog_skill_by_name(pool, viewer_id, reference)
            .await
            .map_err(internal)?
    };
    Ok(skill.ok_or_else(|| no_store(record_not_found(SkillModel::not_found()))))
}

async fn sandbox_skills_index(
    State(state): State<AppState>,
    context: SandboxContext,
    ConsoleQuery(query): ConsoleQuery<SandboxSkillIndexQuery>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let limit = match sandbox_skill_limit(query.limit.as_deref()) {
        Ok(limit) => limit,
        Err(error) => return Ok(error.into_response()),
    };
    let user = sandbox_linked_user(&pool, &context).await?;
    let scope = query
        .scope
        .as_deref()
        .filter(|scope| *scope == "private" || *scope == "shared");
    let skills = db::list_skills(
        &pool,
        user.as_ref().map(|user| user.id),
        db::SkillsTab::Catalog,
        None,
    )
    .await
    .map_err(internal)?;
    let data: Vec<CatalogPayload> = skills
        .iter()
        .filter(|skill| scope.is_none_or(|scope| skill.visibility == scope))
        .take(limit.max(0) as usize)
        .map(|skill| CatalogPayload::new(skill, false))
        .collect();
    Ok(Json(DataPayload { data }).into_response())
}

async fn sandbox_skills_search(
    State(state): State<AppState>,
    context: SandboxContext,
    ConsoleQuery(query): ConsoleQuery<SandboxSkillSearchQuery>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let limit = match sandbox_skill_limit(query.limit.as_deref()) {
        Ok(limit) => limit,
        Err(error) => return Ok(error.into_response()),
    };
    let search = query.q.trim();
    if search.is_empty() {
        return Ok(ConsoleError::bad_request("q must not be empty").into_response());
    }
    let user = sandbox_linked_user(&pool, &context).await?;
    let skills = db::list_skills(
        &pool,
        user.as_ref().map(|user| user.id),
        db::SkillsTab::Catalog,
        Some(search),
    )
    .await
    .map_err(internal)?;
    let data: Vec<CatalogPayload> = skills
        .iter()
        .take(limit.max(0) as usize)
        .map(|skill| CatalogPayload::new(skill, false))
        .collect();
    Ok(Json(DataPayload { data }).into_response())
}

async fn sandbox_skills_show(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let user = sandbox_linked_user(&pool, &context).await?;
    let skill = match sandbox_visible_skill(&pool, user.as_ref().map(|user| user.id), &id).await? {
        Ok(skill) => skill,
        Err(response) => return Ok(response),
    };
    Ok(no_store(
        Json(DataPayload {
            data: CatalogPayload::new(&skill, true),
        })
        .into_response(),
    ))
}

async fn sandbox_skills_create(
    State(state): State<AppState>,
    context: SandboxContext,
    ConsoleData(data): ConsoleData<SkillData>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let Some(user) = sandbox_linked_user(&pool, &context).await? else {
        return Ok(sandbox_authoring_forbidden());
    };
    let draft = skill_draft(&data, false, true);
    // The sandbox never manages editors; a new skill starts with none.
    match db::create_skill(&pool, user.id, draft, &[]).await {
        Ok(skill) => Ok((
            StatusCode::CREATED,
            Json(DataPayload {
                data: sandbox_author_payload(&skill),
            }),
        )
            .into_response()),
        Err(error) => Ok(sandbox_skill_write_error(error)),
    }
}

async fn sandbox_skills_update(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
    ConsoleData(data): ConsoleData<SkillData>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (_, skill) = match sandbox_authored_skill(&pool, &context, &id, true).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    let draft = skill_draft(&data, true, false);
    match db::update_skill(&pool, &skill, draft, None).await {
        Ok(skill) => Ok(Json(DataPayload {
            data: sandbox_author_payload(&skill),
        })
        .into_response()),
        Err(error) => Ok(sandbox_skill_write_error(error)),
    }
}

async fn sandbox_skills_destroy(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (_, skill) = match sandbox_authored_skill(&pool, &context, &id, false).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    db::archive_skill(&pool, skill.id).await.map_err(internal)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn sandbox_skills_share(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (_, skill) = match sandbox_authored_skill(&pool, &context, &id, false).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    sandbox_shared(&pool, &skill, true).await
}

async fn sandbox_skills_unshare(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (_, skill) = match sandbox_authored_skill(&pool, &context, &id, false).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    sandbox_shared(&pool, &skill, false).await
}

async fn sandbox_skills_editors(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let user = sandbox_linked_user(&pool, &context).await?;
    let skill = match sandbox_visible_skill(&pool, user.as_ref().map(|user| user.id), &id).await? {
        Ok(skill) => skill,
        Err(response) => return Ok(response),
    };
    Ok(Json(DataPayload {
        data: sandbox_editor_payload(&pool, &skill).await?,
    })
    .into_response())
}

/// Resolve an editor among active users (adding) or any user (removing, whose
/// scope is the skill's editors).
async fn sandbox_resolve_editor(
    pool: &sqlx::PgPool,
    reference: &str,
    active_only: bool,
) -> Result<Result<db::SkillUser, Response>, ApiError> {
    let user = db::resolve_editor_user(pool, reference.trim(), active_only)
        .await
        .map_err(internal)?;
    Ok(user.ok_or_else(|| no_store(record_not_found(UserModel::not_found()))))
}

/// The editor reference.
#[derive(Debug, serde::Deserialize, garde::Validate)]
struct SkillEditorData {
    // A blank reference is an `is required` error, not a
    // lookup of the empty string.
    #[garde(custom(editor_required))]
    user: String,
}

/// A required reference, reported under its own key.
fn editor_required(value: &str, _: &()) -> garde::Result {
    if value.trim().is_empty() {
        return Err(garde::Error::new("is required"));
    }
    Ok(())
}

async fn sandbox_skills_add_editor(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
    ConsoleData(data): ConsoleData<SkillEditorData>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (_, skill) = match sandbox_authored_skill(&pool, &context, &id, false).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    let reference = match data.validate_fields() {
        Ok(()) => data.user.trim(),
        Err(errors) => return Ok(invalid(errors)),
    };
    let editor = match sandbox_resolve_editor(&pool, reference, true).await? {
        Ok(editor) => editor,
        Err(response) => return Ok(response),
    };
    db::add_skill_editor(&pool, skill.id, editor.id)
        .await
        .map_err(internal)?;
    let Some(skill) = db::skill_by_id(&pool, skill.id).await.map_err(internal)? else {
        return Ok(record_not_found(SkillModel::not_found()));
    };
    Ok(Json(DataPayload {
        data: sandbox_editor_payload(&pool, &skill).await?,
    })
    .into_response())
}

async fn sandbox_skills_remove_editor(
    State(state): State<AppState>,
    context: SandboxContext,
    Path(id): Path<String>,
    ConsoleData(data): ConsoleData<SkillEditorData>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let (_, skill) = match sandbox_authored_skill(&pool, &context, &id, false).await? {
        Ok(found) => found,
        Err(response) => return Ok(response),
    };
    let reference = match data.validate_fields() {
        Ok(()) => data.user.trim(),
        Err(errors) => return Ok(invalid(errors)),
    };
    // The user must already be an editor.
    let editor = match sandbox_resolve_editor(&pool, reference, false).await? {
        Ok(editor) => editor,
        Err(response) => return Ok(response),
    };
    if !db::skill_edited_by(&pool, skill.id, editor.id)
        .await
        .map_err(internal)?
    {
        return Ok(no_store(record_not_found(UserModel::not_found())));
    }
    db::remove_skill_editor(&pool, skill.id, editor.id)
        .await
        .map_err(internal)?;
    let Some(skill) = db::skill_by_id(&pool, skill.id).await.map_err(internal)? else {
        return Ok(record_not_found(SkillModel::not_found()));
    };
    Ok(Json(DataPayload {
        data: sandbox_editor_payload(&pool, &skill).await?,
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Console: /api/v1/console/me/ssh_key
// ---------------------------------------------------------------------------

/// The Active Record encryption keys Console stores secrets with. The Console
/// database crate owns the single environment read; both the secrets area and
/// this personal-key write share it.
fn record_encryption() -> Result<&'static ActiveRecordEncryption, ApiError> {
    centaur_console_db::areas::encryption::active_record_encryption()
        .map_err(|_| ApiError::Internal("Console encryption keys are not configured".to_owned()))
}

/// One grant of the caller's personal SSH key.
#[derive(Serialize)]
struct SshKeyGrantPayload {
    id: String,
    grantee_type: &'static str,
    grantee_id: String,
    label: String,
    principal_kind: Option<String>,
}

impl From<&db::PersonalSshKeyGrant> for SshKeyGrantPayload {
    fn from(grant: &db::PersonalSshKeyGrant) -> Self {
        Self {
            id: grant.oid(),
            grantee_type: if grant.role_id.is_some() {
                "role"
            } else {
                "principal"
            },
            grantee_id: grant.grantee_oid.clone(),
            label: grant.label(),
            principal_kind: grant.principal_kind.clone(),
        }
    }
}

/// The caller's personal SSH key, saved or unsaved.
#[derive(Serialize)]
struct SshKeyPayload {
    id: Option<String>,
    saved: bool,
    name: String,
    has_private_key: bool,
    fingerprint: Option<String>,
    public_key: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    grants: Vec<SshKeyGrantPayload>,
}

impl SshKeyPayload {
    /// The `new`/empty state: no key stored yet.
    fn unsaved() -> Self {
        Self {
            id: None,
            saved: false,
            name: db::PERSONAL_SSH_KEY_DEFAULT_NAME.to_owned(),
            has_private_key: false,
            fingerprint: None,
            public_key: None,
            created_at: None,
            updated_at: None,
            grants: Vec::new(),
        }
    }

    fn new(key: &db::PersonalSshKey, grants: &[db::PersonalSshKeyGrant]) -> Self {
        Self {
            id: Some(key.oid()),
            saved: true,
            name: key.name.clone(),
            has_private_key: key.has_private_key,
            fingerprint: fingerprint(&key.public_key),
            public_key: Some(key.public_key.clone()),
            created_at: Some(key.created_at.clone()),
            updated_at: Some(key.updated_at.clone()),
            grants: grants.iter().map(SshKeyGrantPayload::from).collect(),
        }
    }
}

/// `key_payload`.
async fn ssh_key_payload(pool: &sqlx::PgPool, user_id: i64) -> Result<SshKeyPayload, ApiError> {
    let Some(key) = db::personal_ssh_key(pool, user_id)
        .await
        .map_err(internal)?
    else {
        return Ok(SshKeyPayload::unsaved());
    };
    let grants = db::personal_ssh_key_grants(pool, key.id)
        .await
        .map_err(internal)?;
    Ok(SshKeyPayload::new(&key, &grants))
}

async fn ssh_key_show(
    State(state): State<AppState>,
    user: ConsoleUser,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    Ok(no_store(
        Json(DataPayload {
            data: ssh_key_payload(&pool, user.id).await?,
        })
        .into_response(),
    ))
}

/// A rename keeps the stored private key, and
/// the key itself is only replaced when the body sends a non-empty value.
#[derive(Debug, Default, serde::Deserialize)]
struct PersonalSshKeyData {
    #[serde(default, deserialize_with = "nullable_field")]
    name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    private_key: Option<Option<String>>,
}

/// The key to check without saving.
#[derive(Debug, serde::Deserialize)]
struct SshKeyValidation {
    private_key: String,
}

async fn ssh_key_update(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleData(data): ConsoleData<PersonalSshKeyData>,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let name = data.name.clone().map(|name| name.unwrap_or_default());
    let submitted = data.private_key.clone().flatten();
    let encryption = record_encryption()?;
    match db::save_personal_ssh_key(
        &pool,
        encryption,
        user.id,
        name.as_deref(),
        submitted.as_deref(),
    )
    .await
    {
        Ok(_) => {
            db::apply_personal_ssh_key_grants(&pool, user.id)
                .await
                .map_err(internal)?;
            Ok(no_store(
                Json(DataPayload {
                    data: ssh_key_payload(&pool, user.id).await?,
                })
                .into_response(),
            ))
        }
        // The key's own attributes (`name`, `private_key`, `public_key`) are
        // the ones the SPA reads, so the skill error mapper's rename is inert.
        Err(error) => Ok(no_store(skill_write_error(error))),
    }
}

async fn ssh_key_destroy(
    State(state): State<AppState>,
    user: ConsoleUser,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    db::delete_personal_ssh_key(&pool, user.id)
        .await
        .map_err(internal)?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}

/// `me/ssh_key/validate`'s success payload.
#[derive(Serialize)]
struct SshKeyCheckPayload {
    valid: bool,
    message: &'static str,
    fingerprint: Option<String>,
}

async fn ssh_key_validate(
    _user: ConsoleUser,
    ConsoleData(data): ConsoleData<SshKeyValidation>,
) -> Result<Response, ApiError> {
    match derive_public_key(&data.private_key) {
        Ok(public_key) => Ok(no_store(
            Json(DataPayload {
                data: SshKeyCheckPayload {
                    valid: true,
                    message: "Valid private key. Ready to save.",
                    fingerprint: fingerprint(&public_key),
                },
            })
            .into_response(),
        )),
        Err(message) => Ok(no_store(
            ConsoleError::unprocessable(message.clone())
                .with_details(json!({ "valid": false, "private_key": [message] }))
                .into_response(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Sandbox: /api/v1/sandbox/permissions
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct PrincipalPayload<'a> {
    id: String,
    foreign_id: Option<&'a str>,
    name: Option<&'a str>,
    kind: &'a str,
}

#[derive(serde::Serialize)]
struct CapabilitiesPayload<'a> {
    sandbox_repo_cache: &'a str,
    sandbox_observability_enabled: bool,
    sandbox_sessions_read_enabled: bool,
    sandbox_workflows_read_enabled: bool,
    sandbox_workflows_write_enabled: bool,
}

#[derive(serde::Serialize)]
struct PermissionsData<'a> {
    sandbox_id: &'a str,
    proxy_id: String,
    principal_id: String,
    principal: PrincipalPayload<'a>,
    capabilities: CapabilitiesPayload<'a>,
    slack_channel_permissions: &'a [db::SlackChannelPermissionPayload],
    oauth_credentials: &'a [db::OauthCredentialPayload],
    connected_tools: &'a [String],
    permissions: &'a db::PermissionsPayload,
}

async fn sandbox_permissions_show(
    State(state): State<AppState>,
    context: SandboxContext,
) -> Result<Response, ApiError> {
    let pool = pool(&state)?;
    let permissions = match context.principal_id() {
        Some(principal_id) => db::sandbox_permissions(&pool, principal_id)
            .await
            .map_err(internal)?,
        None => None,
    };
    // A token whose proxy has no principal, or whose principal has no effective
    // config any more, is no longer assigned.
    let Some(permissions) = permissions else {
        return Ok(no_store(
            ConsoleError::new(
                StatusCode::UNAUTHORIZED,
                "sandbox token is no longer assigned",
            )
            .into_response(),
        ));
    };
    let record = &permissions.principal;
    let data = PermissionsData {
        sandbox_id: &context.claims.sandbox_id,
        proxy_id: context.claims.proxy_id.clone(),
        principal_id: model::Principal::oid(record.id),
        principal: PrincipalPayload {
            id: model::Principal::oid(record.id),
            foreign_id: record.foreign_id.as_deref(),
            name: record.name.as_deref(),
            kind: record.kind.as_str(),
        },
        capabilities: CapabilitiesPayload {
            sandbox_repo_cache: record.sandbox_repo_cache.as_str(),
            sandbox_observability_enabled: record.sandbox_observability_enabled,
            sandbox_sessions_read_enabled: record.sandbox_sessions_read_enabled,
            sandbox_workflows_read_enabled: record.sandbox_workflows_read_enabled,
            sandbox_workflows_write_enabled: record.sandbox_workflows_write_enabled,
        },
        slack_channel_permissions: &permissions.slack_channel_permissions,
        oauth_credentials: &permissions.oauth_credentials,
        connected_tools: &permissions.connected_tools,
        permissions: &permissions.permissions,
    };
    Ok(no_store(json_data(data)))
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use serde_json::json;

    use super::*;

    async fn body(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        serde_json::from_slice(&bytes).expect("json body")
    }

    #[tokio::test]
    async fn missing_records_are_not_found() {
        // A model 404 is reported in Console's error envelope instead of a
        // relation where clause.
        for not_found in [
            ScheduledTaskModel::not_found(),
            SkillModel::not_found(),
            UserModel::not_found(),
        ] {
            let response = record_not_found(not_found);
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert!(body(response).await["error"]["message"].is_string());
        }
    }

    #[tokio::test]
    async fn a_missing_run_runtime_uses_the_error_envelope() {
        // A missing runtime always renders Console's error envelope.
        let response = run_error(&RunFailure::Unavailable);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn task_data_is_typed_and_keeps_absent_apart_from_null() {
        let data: ConsoleTaskData =
            serde_json::from_value(json!({ "name": null, "unknown": 1 })).expect("typed data");
        assert_eq!(data.name, Some(None));
        assert_eq!(data.enabled, None);
        assert_eq!(data.custom_days, None);
        // No casting: a string is not a boolean, and a scalar is not
        // an array of days. The extractor reports these under the field path.
        for value in [json!({ "enabled": "true" }), json!({ "custom_days": "1" })] {
            assert!(serde_json::from_value::<ConsoleTaskData>(value).is_err());
        }
    }

    #[test]
    fn skill_lock_versions_are_integers() {
        let data: SkillData =
            serde_json::from_value(json!({ "lock_version": null })).expect("typed data");
        assert_eq!(data.lock_version, Some(None));
        assert!(serde_json::from_value::<SkillData>(json!({ "lock_version": "1" })).is_err());
    }

    fn skill() -> db::Skill {
        db::Skill {
            id: 7,
            user_id: 3,
            name: "demo".to_owned(),
            description: "a skill".to_owned(),
            content: "# Docs".to_owned(),
            visibility: "shared".to_owned(),
            lock_version: 2,
            created_at: "2026-01-10T12:00:00Z".to_owned(),
            updated_at: "2026-01-10T12:01:00Z".to_owned(),
            author_name: Some("Ada".to_owned()),
            author_email: "ada@example.test".to_owned(),
        }
    }

    #[test]
    fn a_skill_detail_replaces_the_summary_viewer_flags() {
        let skill = skill();
        let detail = serde_json::to_value(skill_detail(&skill, 3, false, &[])).unwrap();
        assert_eq!(
            detail,
            json!({
                "id": SkillModel::oid(7),
                "name": "demo",
                "description": "a skill",
                "visibility": "shared",
                "author": "Ada",
                "owner": {
                    "id": UserModel::oid(3),
                    "name": "Ada",
                    "email": "ada@example.test",
                },
                "updated_at": "2026-01-10T12:01:00Z",
                "viewer": {
                    "owner": true,
                    "can_edit": true,
                    "can_manage": true,
                    "can_unshare": true,
                },
                "instructions": "# Docs",
                "lock_version": 2,
                "checksum": db::skill_checksum(
                    &skill.name,
                    &skill.description,
                    &skill.content
                ),
                "created_at": "2026-01-10T12:00:00Z",
                "editors": [],
            })
        );

        // The summary is the same payload without the document fields.
        let summary = serde_json::to_value(skill_summary_of(&skill, 4)).unwrap();
        assert_eq!(
            summary,
            json!({
                "id": SkillModel::oid(7),
                "name": "demo",
                "description": "a skill",
                "visibility": "shared",
                "author": "Ada",
                "owner": {
                    "id": UserModel::oid(3),
                    "name": "Ada",
                    "email": "ada@example.test",
                },
                "updated_at": "2026-01-10T12:01:00Z",
                "viewer": { "owner": false },
            })
        );
    }

    /// `SkillSummaryPayload` without the flattened detail wrapper.
    fn skill_summary_of(skill: &db::Skill, viewer_id: i64) -> serde_json::Value {
        serde_json::to_value(SkillSummaryPayload::new(skill, viewer_id)).unwrap()
    }

    #[test]
    fn catalog_payloads_only_carry_the_document_when_asked() {
        let skill = skill();
        let catalog = serde_json::to_value(CatalogPayload::new(&skill, false)).unwrap();
        assert_eq!(
            catalog,
            json!({
                "id": SkillModel::oid(7),
                "name": "demo",
                "description": "a skill",
                "visibility": "shared",
                "author": "Ada",
                "updated_at": "2026-01-10T12:01:00Z",
                "checksum": db::skill_checksum(
                    &skill.name,
                    &skill.description,
                    &skill.content
                ),
            })
        );
        let author = serde_json::to_value(sandbox_author_payload(&skill)).unwrap();
        assert_eq!(author["lock_version"], json!(2));
        assert_eq!(
            author["document"],
            json!(db::skill_document(
                &skill.name,
                &skill.description,
                &skill.content
            ))
        );
    }

    fn run() -> db::WorkflowRunRow {
        db::WorkflowRunRow {
            queue_name: Some("centaur_workflows_etl".to_owned()),
            run_id: Some("run-1".to_owned()),
            task_id: Some("task-1".to_owned()),
            task_name: Some("centaur_workflow".to_owned()),
            workflow_name: Some("wf-a".to_owned()),
            harness_type: Some("codex".to_owned()),
            state: Some("completed".to_owned()),
            attempts: Some(1),
            max_attempts: Some(3),
            claimed: Some(false),
            created_at: Some("2026-01-10T12:00:00.000Z".to_owned()),
            first_started_at: None,
            started_at: Some("2026-01-10T11:55:00.000Z".to_owned()),
            completed_at: Some("2026-01-10T11:50:00.000Z".to_owned()),
            failed_at: None,
            cancelled_at: None,
            available_at: None,
            queue_run_count: Some(2),
        }
    }

    #[test]
    fn workflow_rows_flatten_the_run_timing() {
        let run = run();
        let queues = vec![run.clone()];
        let row = serde_json::to_value(workflow_row(&run, Some(&queues))).unwrap();
        assert_eq!(
            row,
            json!({
                "workflow_key": "wf-a",
                "label": "wf-a",
                "task_name": "centaur_workflow",
                "status": "completed",
                "queue_name": "centaur_workflows_etl",
                "queue_label": "etl",
                "attempts": 1,
                "max_attempts": 3,
                "started_at": "2026-01-10T11:55:00.000Z",
                "created_at": "2026-01-10T12:00:00.000Z",
                "finished_at": "2026-01-10T11:50:00.000Z",
                "queues": [{
                    "queue_name": "centaur_workflows_etl",
                    "queue_label": "etl",
                    "status": "completed",
                    "last_activity_at": "2026-01-10T11:50:00.000Z",
                    "run_count": 2,
                }],
            })
        );

        // A run with no queue breakdown keeps an empty `queues` list.
        let row = serde_json::to_value(workflow_row(&run, None)).unwrap();
        assert_eq!(row["queues"], json!([]));
    }

    #[test]
    fn overview_carries_the_schedule_and_debug_blocks() {
        let run = run();
        let status_counts = std::collections::BTreeMap::from([
            ("completed".to_owned(), 2_i64),
            ("failed".to_owned(), 1_i64),
        ]);
        let queue_names = vec!["centaur_workflows".to_owned()];
        let debug = vec![debug_row("Input", json!({ "a": 1 }), None)];
        let payload = serde_json::to_value(overview(
            "wf-a",
            &run,
            &status_counts,
            &queue_names,
            None,
            debug,
        ))
        .unwrap();
        assert_eq!(payload["workflow_key"], json!("wf-a"));
        assert_eq!(payload["engine_label"], json!("Codex"));
        // The fixture finishes before it starts, which reads as instant.
        assert_eq!(payload["duration_label"], json!("less than a minute"));
        assert_eq!(payload["run_count"], json!(3));
        assert_eq!(
            payload["status_counts"],
            json!([
                { "status": "completed", "count": 2 },
                { "status": "failed", "count": 1 },
            ])
        );
        assert_eq!(
            payload["queues"],
            json!([{ "queue_name": "centaur_workflows", "queue_label": "default" }])
        );
        assert_eq!(payload["schedule"], Value::Null);
        assert_eq!(
            payload["debug"],
            json!([{ "label": "Input", "value": { "a": 1 }, "run_id": Value::Null }])
        );

        let schedule = json!({
            "kind": { "type": "cron", "cron": "0 9 * * *" },
            "timezone": "UTC",
            "source_path": "centaur-tempo/wf.yaml",
        });
        let payload = serde_json::to_value(overview(
            "wf-a",
            &run,
            &status_counts,
            &[],
            Some(&schedule),
            Vec::new(),
        ))
        .unwrap();
        assert_eq!(
            payload["schedule"],
            json!({
                "label": "cron 0 9 * * *",
                "summary": "cron 0 9 * * * · UTC",
                "timezone": "UTC",
                "enabled": true,
                "source_path": "centaur-tempo/wf.yaml",
                "source_url": "https://github.com/tempoxyz/centaur-tempo/blob/main/wf.yaml",
            })
        );
    }

    #[test]
    fn requested_owner_is_only_me_and_only_on_create() {
        let data = |owner: Value| SandboxTaskData {
            owner,
            ..SandboxTaskData::default()
        };
        assert!(!requested_owner(None, false).unwrap());
        assert!(!requested_owner(Some(&data(Value::Null)), true).unwrap());
        assert!(requested_owner(Some(&data(json!("me"))), true).unwrap());

        let invalid = requested_owner(Some(&data(json!("U2222222222"))), true).unwrap_err();
        assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(invalid.message, "owner must be \"me\"");

        let later = requested_owner(Some(&data(json!("me"))), false).unwrap_err();
        assert_eq!(later.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(later.message, "owner can only be set when creating a task");
    }

    #[tokio::test]
    async fn a_blank_editor_reference_is_a_required_user_error() {
        for blank in ["", " ", "\n\t"] {
            let data: SkillEditorData =
                serde_json::from_value(json!({ "user": blank })).expect("typed data");
            let response = invalid(data.validate_fields().unwrap_err());
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
        // A reference is trimmed before it is looked up, so surrounding space
        // is not blank.
        let data: SkillEditorData =
            serde_json::from_value(json!({ "user": " usr_1 " })).expect("typed data");
        assert!(data.validate_fields().is_ok());
        assert_eq!(data.user.trim(), "usr_1");
    }

    #[test]
    fn ssh_key_payloads_distinguish_saved_from_unsaved() {
        assert_eq!(
            serde_json::to_value(SshKeyPayload::unsaved()).unwrap(),
            json!({
                "id": Value::Null,
                "saved": false,
                "name": db::PERSONAL_SSH_KEY_DEFAULT_NAME,
                "has_private_key": false,
                "fingerprint": Value::Null,
                "public_key": Value::Null,
                "created_at": Value::Null,
                "updated_at": Value::Null,
                "grants": [],
            })
        );
        let key = db::PersonalSshKey {
            id: 9,
            name: "My key".to_owned(),
            public_key: "not-a-key".to_owned(),
            created_at: "2026-01-10T12:00:00Z".to_owned(),
            updated_at: "2026-01-10T12:01:00Z".to_owned(),
            has_private_key: true,
        };
        let grant = db::PersonalSshKeyGrant {
            id: 11,
            role_id: None,
            principal_id: Some(5),
            grantee_oid: model::Principal::oid(5),
            grantee_name: Some("Channel".to_owned()),
            grantee_foreign_id: Some("slack-channel-1".to_owned()),
            principal_kind: Some("slack_channel".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(SshKeyPayload::new(&key, &[grant])).unwrap(),
            json!({
                "id": model::SshKeySecret::oid(9),
                "saved": true,
                "name": "My key",
                "has_private_key": true,
                "fingerprint": Value::Null,
                "public_key": "not-a-key",
                "created_at": "2026-01-10T12:00:00Z",
                "updated_at": "2026-01-10T12:01:00Z",
                "grants": [{
                    "id": model::Grant::oid(11),
                    "grantee_type": "principal",
                    "grantee_id": model::Principal::oid(5),
                    "label": "Channel",
                    "principal_kind": "slack_channel",
                }],
            })
        );
    }
}
