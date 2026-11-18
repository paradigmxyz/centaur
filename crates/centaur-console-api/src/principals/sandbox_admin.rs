//! What only the sandbox admin namespace adds to the shared endpoints:
//! finding principals by Slack channel, creating the principals api-rs
//! derives from sessions, sandbox access, and the Slack Connect allowlist
//! (kept in api-rs).

use std::time::Duration;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use centaur_console_auth::tokens;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    Actor, FieldErrors, Grantee, check_foreign_id,
    effects::{self, Committed},
    payload,
    permissions::{self, PermissionInput},
    principal::{self, Fields, ListQuery, PrincipalInput, Saved, Target},
};
use crate::{
    ApiError, ApiResult, AppState,
    auth::SandboxAdminCaller,
    extract::{Data, Path, Query, data},
    http::without_url,
    pagination::PageParams,
};

#[derive(Deserialize)]
pub struct ChannelFilter {
    slack_channel_id: Option<String>,
}

/// `GET /principals`: with `slack_channel_id`, the principals bound to that
/// channel (unpaginated); otherwise the usual list.
pub async fn principals(
    State(state): State<AppState>,
    Query(channel): Query<ChannelFilter>,
    Query(list): Query<ListQuery>,
    Query(page): Query<PageParams>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> ApiResult {
    let channel_id = channel
        .slack_channel_id
        .map(|id| id.trim().to_uppercase())
        .filter(|id| !id.is_empty());
    let Some(channel_id) = channel_id else {
        return principal::index(State(state), Query(list), Query(page), Query(pairs)).await;
    };
    let rows = sqlx::query_as(&format!(
        "SELECT {} FROM principals WHERE slack_channel_id = $1 ORDER BY id",
        payload::PRINCIPAL_COLUMNS
    ))
    .bind(channel_id)
    .fetch_all(&state.db)
    .await?;
    Ok(data(payload::principals_json(&state.db, rows).await?).into_response())
}

#[derive(Deserialize)]
pub struct NewPrincipal {
    kind: Option<String>,
    slack_channel_id: Option<String>,
    slack_user_id: Option<String>,
    workflow_name: Option<String>,
}

/// api-rs's slug of a workflow name: lower-case alphanumerics, runs of
/// anything else become one `-`, trimmed; `x` when nothing is left.
fn workflow_slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "x".into()
    } else {
        slug.to_owned()
    }
}

fn upper(value: &Option<String>) -> String {
    value.as_deref().unwrap_or_default().trim().to_uppercase()
}

/// `POST /principals`: a Slack channel, Slack DM, or workflow principal;
/// the foreign id is derived the way api-rs derives it, so the two agree.
pub async fn create_principal(
    State(state): State<AppState>,
    Actor(actor): Actor,
    SandboxAdminCaller(auth): SandboxAdminCaller,
    Data(input): Data<NewPrincipal>,
) -> ApiResult {
    let mut fields = Fields::defaults(&mut *state.db.acquire().await?).await?;
    let (foreign_id, channel_permission) = match input.kind.as_deref() {
        Some("slack_channel") => {
            let id = upper(&input.slack_channel_id);
            if !(id.starts_with('C') || id.starts_with('G')) {
                return Err(ApiError::BadRequest(
                    "slack_channel_id must be a C… or G… channel ID".into(),
                ));
            }
            fields.kind = "slack_channel".into();
            fields.name = Some(format!("Slack Channel {id}"));
            fields.slack_channel_id = Some(id.clone());
            (format!("slack-channel-{}", id.to_lowercase()), Some(id))
        }
        Some("slack_dm") => {
            let id = upper(&input.slack_user_id);
            let team = auth.sandbox.slack_team_id.clone().unwrap_or_default();
            fields.kind = "slack_dm".into();
            fields.name = Some(format!("Slack User {id} (team {team})"));
            fields.slack_user_id = Some(id.clone());
            fields.slack_team_id = Some(team.clone());
            (
                format!("slack-user-{}-{}", team.to_lowercase(), id.to_lowercase()),
                None,
            )
        }
        Some("workflow") => {
            let name = input
                .workflow_name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| {
                    ApiError::BadRequest("workflow_name must be a WORKFLOW_NAME".into())
                })?;
            fields.kind = "workflow".into();
            fields.name = Some(format!("Workflow {name}"));
            fields.labels = json!({"managed-by": "centaur", "workflow_name": name});
            (format!("workflow-{}", workflow_slug(name)), None)
        }
        _ => {
            return Err(ApiError::BadRequest(
                "kind must be slack_channel, slack_dm, or workflow".into(),
            ));
        }
    };
    fields.sync_repo_cache_label();
    let mut errors: FieldErrors = fields.validate(None);
    check_foreign_id(&mut errors, Some(&foreign_id), "prn");
    errors.finish()?;

    let now = state.now();
    let mut tx = state.db.begin().await?;
    let id = principal::insert(&mut tx, actor, Some(&foreign_id), &fields, now)
        .await?
        .ok_or_else(|| ApiError::field("foreign_id", "has already been taken"))?;
    // api-rs adds this only to principals it creates itself.
    if let Some(channel_id) = channel_permission {
        let all_on = PermissionInput {
            channel_id,
            ..PermissionInput::default()
        };
        permissions::upsert_in(&mut tx, Grantee::Principal(id), &all_on, now).await?;
    }
    tx.commit().await?;
    let saved = Saved {
        id,
        created: true,
        identity_changed: true,
        slack_channel_id: fields
            .slack_channel_id
            .clone()
            .filter(|_| fields.kind == "slack_channel"),
    };
    effects::principal_committed(&state, Committed::from(&saved)).await;
    Ok((
        StatusCode::CREATED,
        data(payload::principal_json(&state.db, id).await?),
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct SandboxAccess {
    sandbox_repo_cache: Option<String>,
    sandbox_observability_enabled: Option<bool>,
    sandbox_sessions_read_enabled: Option<bool>,
    sandbox_workflows_read_enabled: Option<bool>,
    sandbox_workflows_write_enabled: Option<bool>,
}

/// `PATCH /principals/{id}/sandbox_access`: some of the sandbox
/// capabilities, as literal booleans (and a repo cache mode).
pub async fn sandbox_access(
    State(state): State<AppState>,
    Actor(actor): Actor,
    Path(id): Path<String>,
    Data(access): Data<SandboxAccess>,
) -> ApiResult {
    if access.sandbox_repo_cache.is_none()
        && access.sandbox_observability_enabled.is_none()
        && access.sandbox_sessions_read_enabled.is_none()
        && access.sandbox_workflows_read_enabled.is_none()
        && access.sandbox_workflows_write_enabled.is_none()
    {
        return Err(ApiError::BadRequest(
            "provide at least one sandbox capability".into(),
        ));
    }
    let input = PrincipalInput {
        sandbox_repo_cache: access.sandbox_repo_cache,
        sandbox_observability_enabled: access.sandbox_observability_enabled,
        sandbox_sessions_read_enabled: access.sandbox_sessions_read_enabled,
        sandbox_workflows_read_enabled: access.sandbox_workflows_read_enabled,
        sandbox_workflows_write_enabled: access.sandbox_workflows_write_enabled,
        ..PrincipalInput::default()
    };
    let target = Target::Id(principal::principal_id(&id)?);
    let saved = principal::write(&state, actor, target, &input).await?;
    principal::respond(&state, saved, false).await
}

// --- Slack Connect allowlist -------------------------------------------------

/// Why the allowlist could not be read or changed.
enum TeamsError {
    /// api-rs refused the ids (HTTP 400): its message.
    Rejected(String),
    /// api-rs could not be reached or failed.
    Unavailable,
}

fn teams_failed(error: TeamsError) -> ApiResult<Response> {
    match error {
        TeamsError::Rejected(message) => Err(ApiError::Unprocessable {
            message,
            reason: None,
        }),
        TeamsError::Unavailable => Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"error": {"message": "the Centaur API is unavailable"}})),
        )
            .into_response()),
    }
}

/// `GET`/`PUT /api/slack/external-team-ids` on api-rs; the answer's ids.
async fn teams_request(
    state: &AppState,
    replace_with: Option<&[String]>,
) -> Result<Vec<String>, TeamsError> {
    let unavailable = |what: &str| {
        tracing::warn!("sandbox_admin_slack_external_teams_failed: {what}");
        TeamsError::Unavailable
    };
    let token = tokens::console_service(&state.config.jwt_signing_secret, state.now())
        .map_err(|_| unavailable("the service token could not be minted"))?;
    let base = state
        .config
        .env
        .var_present("CENTAUR_API_URL")
        .unwrap_or("http://localhost:8080")
        .trim_end_matches('/');
    let url = format!("{base}/api/slack/external-team-ids");
    let request = match replace_with {
        Some(ids) => state
            .http
            .put(url)
            .json(&json!({"slack_external_team_ids": ids})),
        None => state.http.get(url),
    };
    let response = request
        .bearer_auth(token)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|error| unavailable(&without_url(error).to_string()))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if status == StatusCode::BAD_REQUEST {
        let message = ["error", "message", "detail"]
            .iter()
            .find_map(|key| body.get(key).and_then(Value::as_str))
            .unwrap_or("Centaur API rejected the Slack team ids");
        return Err(TeamsError::Rejected(message.to_owned()));
    }
    if !status.is_success() {
        return Err(unavailable(&format!("api-rs answered HTTP {status}")));
    }
    Ok(body
        .get("slack_external_team_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default())
}

/// `GET /slack_external_teams`.
pub async fn teams(State(state): State<AppState>) -> ApiResult {
    match teams_request(&state, None).await {
        Ok(ids) => Ok(data(ids).into_response()),
        Err(error) => teams_failed(error),
    }
}

#[derive(Deserialize)]
pub struct NewTeam {
    team_id: String,
}

/// `POST /slack_external_teams`: allows a workspace (`T…`) or Enterprise
/// Grid org (`E…`); allowing one twice is not an error.
pub async fn add_team(State(state): State<AppState>, Data(input): Data<NewTeam>) -> ApiResult {
    let team_id = input.team_id.trim().to_uppercase();
    let result = async {
        let mut ids = teams_request(&state, None).await?;
        if !ids.contains(&team_id) {
            ids.push(team_id.clone());
            ids = teams_request(&state, Some(&ids)).await?;
        }
        Ok(ids)
    }
    .await;
    match result {
        Ok(ids) => Ok(data(ids).into_response()),
        Err(error) => teams_failed(error),
    }
}

/// `DELETE /slack_external_teams/{team_id}`.
pub async fn remove_team(State(state): State<AppState>, Path(team_id): Path<String>) -> ApiResult {
    let team_id = team_id.trim().to_uppercase();
    let result = async {
        let mut ids = teams_request(&state, None).await?;
        if !ids.contains(&team_id) {
            return Ok(None);
        }
        ids.retain(|id| *id != team_id);
        Ok(Some(teams_request(&state, Some(&ids)).await?))
    }
    .await;
    match result {
        Ok(Some(ids)) => Ok(data(ids).into_response()),
        Ok(None) => Err(ApiError::NotFound(format!(
            "{team_id} is not an allowed Slack Connect workspace"
        ))),
        Err(error) => teams_failed(error),
    }
}
