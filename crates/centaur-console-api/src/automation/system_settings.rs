//! System settings: the deployment-wide defaults for new principals, the
//! roles assigned to them by default, and the Slack Connect workspace
//! allowlist (stored by api-rs). Admin only.

use axum::{extract::State, http::StatusCode, response::IntoResponse};
use centaur_console_db::{decode_oid, oid};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use super::centaur_api::CentaurApiClient;
use crate::{
    ApiError, ApiResult, AppState, FieldErrors,
    auth::ConsoleCaller,
    extract::{Data, data},
};

const REPO_CACHE_VALUES: [&str; 3] = ["none", "public", "all"];

#[derive(sqlx::FromRow)]
struct Settings {
    default_sandbox_repo_cache: String,
    default_sandbox_observability_enabled: bool,
    default_sandbox_sessions_read_enabled: bool,
    default_sandbox_workflows_read_enabled: bool,
    default_sandbox_workflows_write_enabled: bool,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct RoleRow {
    id: i64,
    foreign_id: Option<String>,
    name: String,
    assign_by_default: bool,
}

/// The settings row, created with the column defaults when missing.
async fn current(state: &AppState) -> Result<Settings, sqlx::Error> {
    let now = state.now().naive_utc();
    sqlx::query(
        "INSERT INTO system_settings (singleton, created_at, updated_at) VALUES (TRUE, $1, $1) \
         ON CONFLICT DO NOTHING",
    )
    .bind(now)
    .execute(&state.db)
    .await?;
    sqlx::query_as(
        "SELECT default_sandbox_repo_cache, default_sandbox_observability_enabled, \
                default_sandbox_sessions_read_enabled, default_sandbox_workflows_read_enabled, \
                default_sandbox_workflows_write_enabled, \
                updated_at AT TIME ZONE 'UTC' AS updated_at \
         FROM system_settings ORDER BY id LIMIT 1",
    )
    .fetch_one(&state.db)
    .await
}

async fn render(state: &AppState, settings: &Settings) -> ApiResult {
    let roles: Vec<RoleRow> = sqlx::query_as(
        "SELECT id, foreign_id, name, assign_by_default FROM roles ORDER BY name, foreign_id, id",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(data(json!({
        "default_sandbox_repo_cache": settings.default_sandbox_repo_cache,
        "default_sandbox_observability_enabled": settings.default_sandbox_observability_enabled,
        "default_sandbox_sessions_read_enabled": settings.default_sandbox_sessions_read_enabled,
        "default_sandbox_workflows_read_enabled": settings.default_sandbox_workflows_read_enabled,
        "default_sandbox_workflows_write_enabled": settings.default_sandbox_workflows_write_enabled,
        "default_role_ids": roles
            .iter()
            .filter(|role| role.assign_by_default)
            .map(|role| oid("role", role.id))
            .collect::<Vec<_>>(),
        "roles": roles
            .iter()
            .map(|role| json!({
                "id": oid("role", role.id),
                "foreign_id": role.foreign_id,
                "name": role.name,
                "assign_by_default": role.assign_by_default,
            }))
            .collect::<Vec<_>>(),
        "updated_at": settings.updated_at,
    }))
    .into_response())
}

pub async fn show(State(state): State<AppState>, _: ConsoleCaller) -> ApiResult {
    let settings = current(&state).await?;
    render(&state, &settings).await
}

#[derive(Default, Deserialize)]
pub struct Update {
    default_sandbox_repo_cache: Option<String>,
    default_sandbox_observability_enabled: Option<bool>,
    default_sandbox_sessions_read_enabled: Option<bool>,
    default_sandbox_workflows_read_enabled: Option<bool>,
    default_sandbox_workflows_write_enabled: Option<bool>,
    /// Exactly these roles are assigned to new principals by default.
    default_role_ids: Option<Vec<String>>,
}

pub async fn update(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Data(input): Data<Update>,
) -> ApiResult {
    let mut errors = FieldErrors::new();
    if let Some(cache) = &input.default_sandbox_repo_cache
        && !REPO_CACHE_VALUES.contains(&cache.as_str())
    {
        errors.add("default_sandbox_repo_cache", "is not included in the list");
    }
    // Role ids are checked against the roles that exist.
    let mut role_ids = None;
    if let Some(requested) = &input.default_role_ids {
        let mut oids: Vec<&str> = requested
            .iter()
            .map(|oid| oid.trim())
            .filter(|oid| !oid.is_empty())
            .collect();
        oids.sort_unstable();
        oids.dedup();
        let decoded: Vec<Option<i64>> = oids.iter().map(|oid| decode_oid("role", oid)).collect();
        let found: Vec<i64> = sqlx::query_scalar("SELECT id FROM roles WHERE id = ANY($1)")
            .bind(decoded.iter().flatten().copied().collect::<Vec<_>>())
            .fetch_all(&state.db)
            .await?;
        let unknown: Vec<&str> = oids
            .iter()
            .zip(&decoded)
            .filter(|(_, id)| !id.is_some_and(|id| found.contains(&id)))
            .map(|(oid, _)| *oid)
            .collect();
        if !unknown.is_empty() {
            errors.add(
                "default_role_ids",
                format!("contains unknown roles: {}", unknown.join(", ")),
            );
        }
        role_ids = Some(found);
    }
    errors.finish()?;

    current(&state).await?;
    let now = state.now().naive_utc();
    let mut tx = state.db.begin().await?;
    let any_default = input.default_sandbox_repo_cache.is_some()
        || input.default_sandbox_observability_enabled.is_some()
        || input.default_sandbox_sessions_read_enabled.is_some()
        || input.default_sandbox_workflows_read_enabled.is_some()
        || input.default_sandbox_workflows_write_enabled.is_some();
    if any_default {
        sqlx::query(
            "UPDATE system_settings SET \
               default_sandbox_repo_cache = COALESCE($1, default_sandbox_repo_cache), \
               default_sandbox_observability_enabled = COALESCE($2, default_sandbox_observability_enabled), \
               default_sandbox_sessions_read_enabled = COALESCE($3, default_sandbox_sessions_read_enabled), \
               default_sandbox_workflows_read_enabled = COALESCE($4, default_sandbox_workflows_read_enabled), \
               default_sandbox_workflows_write_enabled = COALESCE($5, default_sandbox_workflows_write_enabled), \
               updated_at = $6 \
             WHERE singleton",
        )
        .bind(input.default_sandbox_repo_cache)
        .bind(input.default_sandbox_observability_enabled)
        .bind(input.default_sandbox_sessions_read_enabled)
        .bind(input.default_sandbox_workflows_read_enabled)
        .bind(input.default_sandbox_workflows_write_enabled)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    if let Some(role_ids) = role_ids {
        sqlx::query(
            "UPDATE roles SET assign_by_default = (id = ANY($1)), updated_at = $2 \
             WHERE assign_by_default <> (id = ANY($1))",
        )
        .bind(role_ids)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let settings = current(&state).await?;
    render(&state, &settings).await
}

// --- Slack Connect workspaces (stored by api-rs) -----------------------------

fn teams(ids: Vec<Value>) -> axum::response::Response {
    data(json!({ "slack_external_team_ids": ids })).into_response()
}

pub async fn slack_external_teams(State(state): State<AppState>, _: ConsoleCaller) -> ApiResult {
    let client = CentaurApiClient::with_timeout(&state, std::time::Duration::from_secs(5));
    match client.slack_external_team_ids().await {
        Ok(ids) => Ok(teams(ids)),
        Err(error) => {
            tracing::warn!(%error, "console_slack_external_team_ids_load_failed");
            Err(ApiError::upstream(
                StatusCode::SERVICE_UNAVAILABLE,
                "The allowed external workspaces could not be loaded from the Centaur API.",
                None,
            ))
        }
    }
}

#[derive(Deserialize)]
pub struct TeamIds {
    /// A list, or one string split on whitespace and commas.
    #[serde(default)]
    slack_external_team_ids: TeamIdsInput,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TeamIdsInput {
    List(Vec<String>),
    Text(String),
}

impl Default for TeamIdsInput {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

impl TeamIdsInput {
    fn ids(self) -> Vec<String> {
        let list = match self {
            Self::List(list) => list,
            Self::Text(text) => text
                .split(|c: char| c.is_whitespace() || c == ',')
                .map(str::to_owned)
                .collect(),
        };
        list.into_iter()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
            .collect()
    }
}

pub async fn replace_slack_external_teams(
    State(state): State<AppState>,
    _: ConsoleCaller,
    Data(input): Data<TeamIds>,
) -> ApiResult {
    let client = CentaurApiClient::with_timeout(&state, std::time::Duration::from_secs(5));
    match client
        .replace_slack_external_team_ids(&input.slack_external_team_ids.ids())
        .await
    {
        Ok(ids) => Ok(teams(ids)),
        // api-rs rejected the ids themselves.
        Err(error) if error.status == Some(400) => Err(ApiError::Unprocessable {
            message: error.message,
            reason: None,
        }),
        Err(error) => {
            tracing::warn!(%error, "console_slack_external_team_ids_save_failed");
            Err(ApiError::upstream(
                StatusCode::SERVICE_UNAVAILABLE,
                "Slack Connect workspaces could not be saved because the Centaur API is unavailable.",
                None,
            ))
        }
    }
}
