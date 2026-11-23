//! Console's System Settings page and the Slack Connect allowlist.
//!
//! The settings row is a singleton with deployment-wide defaults for new
//! principals; `default_role_ids` replaces the set of roles new principals
//! receive. The allowlist itself lives with the session store, which is what
//! Slackbot reads, so these handlers edit it through [`PgSessionStore`].

use axum::{extract::State, http::StatusCode, response::Response, routing::get};
use centaur_console_db::ConsoleUser;
use centaur_console_db::areas::admin::{
    RoleRow, SystemSettingValues, existing_role_ids, roles, system_setting, write_system_settings,
};
use centaur_console_db::model::{ConsoleModel, Role as RoleModel};
use centaur_console_db::validation::{ValidateFields, rules};
use centaur_session_sqlx::PgSessionStore;
use serde::{Deserialize, Serialize};
use time::PrimitiveDateTime;

use super::common::{self, nullable_field};
use crate::{
    AppState,
    console::guard::GuardedRoutes,
    console::{
        http::{ConsoleError, json_data},
        request::ConsoleData,
    },
    slack_external_teams::normalize_slack_external_team_ids,
};

const REPO_CACHE_VALUES: [&str; 3] = ["none", "public", "all"];

/// A settings write. An absent key leaves its column alone; an explicit `null`
/// fails the model's inclusion validation, except for `default_role_ids`, where
/// it clears the default set.
#[derive(Deserialize, garde::Validate)]
struct UpdateSystemSettings {
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(custom(rules::patch_included_in(&REPO_CACHE_VALUES)))]
    default_sandbox_repo_cache: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(inner(required))]
    default_sandbox_observability_enabled: Option<Option<bool>>,
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(inner(required))]
    default_sandbox_sessions_read_enabled: Option<Option<bool>>,
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(inner(required))]
    default_sandbox_workflows_read_enabled: Option<Option<bool>>,
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(inner(required))]
    default_sandbox_workflows_write_enabled: Option<Option<bool>>,
    /// Replaces the default role set; the handler resolves the references
    /// against the roles table.
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(skip)]
    default_role_ids: Option<Option<Vec<String>>>,
}

/// `PUT slack_external_teams` accepts a list, or one string split on whitespace
/// and commas (the textarea form).
#[derive(Deserialize)]
struct ReplaceSlackExternalTeams {
    #[serde(default)]
    slack_external_team_ids: Option<SlackTeamIdInput>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SlackTeamIdInput {
    List(Vec<String>),
    Text(String),
}

/// The System Settings document: the stored defaults, flattened from the row
/// projection, plus the default roles and every role's flag.
#[derive(Serialize)]
struct SettingsData {
    #[serde(flatten)]
    values: SystemSettingValues,
    default_role_ids: Vec<String>,
    roles: Vec<SettingsRole>,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    updated_at: PrimitiveDateTime,
}

#[derive(Serialize)]
struct SettingsRole {
    id: String,
    foreign_id: Option<String>,
    name: Option<String>,
    assign_by_default: bool,
}

impl From<&RoleRow> for SettingsRole {
    fn from(role: &RoleRow) -> Self {
        Self {
            id: RoleModel::oid(role.id),
            foreign_id: role.foreign_id.clone(),
            name: role.name.clone(),
            assign_by_default: role.assign_by_default,
        }
    }
}

#[derive(Serialize)]
struct SlackExternalTeamIds {
    slack_external_team_ids: Vec<String>,
}

pub(super) fn console(routes: GuardedRoutes) -> GuardedRoutes {
    routes
        .console_admin("/system_settings", get(show).patch(update))
        .console_admin(
            "/system_settings/slack_external_teams",
            get(slack_external_teams).put(replace_slack_external_teams),
        )
}

async fn show(State(state): State<AppState>, _user: ConsoleUser) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    Ok(json_data(settings_payload(&pool).await?))
}

async fn update(
    State(state): State<AppState>,
    _user: ConsoleUser,
    ConsoleData(data): ConsoleData<UpdateSystemSettings>,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let setting = system_setting(&pool).await?;
    // Console resolves the roles before it checks the settings, so an unknown
    // role wins over a bad setting.
    let replace_roles = match &data.default_role_ids {
        Some(role_ids) => Some(
            resolve_default_role_ids(
                &pool,
                &default_role_oids(role_ids.clone().unwrap_or_default()),
            )
            .await?,
        ),
        None => None,
    };
    data.validate_fields()?;
    let mut values = setting.values.clone();
    apply_settings(&mut values, &data);
    write_system_settings(&pool, setting.id, &values, replace_roles.as_deref()).await?;
    Ok(json_data(settings_payload(&pool).await?))
}

/// Copy the settings the request carries onto the stored values. Only fields
/// that passed their rule are `Some`, and the rule rejected an explicit `null`.
fn apply_settings(values: &mut SystemSettingValues, data: &UpdateSystemSettings) {
    if let Some(Some(repo_cache)) = &data.default_sandbox_repo_cache {
        values.default_sandbox_repo_cache = repo_cache.clone();
    }
    for (stored, value) in [
        (
            &mut values.default_sandbox_observability_enabled,
            data.default_sandbox_observability_enabled,
        ),
        (
            &mut values.default_sandbox_sessions_read_enabled,
            data.default_sandbox_sessions_read_enabled,
        ),
        (
            &mut values.default_sandbox_workflows_read_enabled,
            data.default_sandbox_workflows_read_enabled,
        ),
        (
            &mut values.default_sandbox_workflows_write_enabled,
            data.default_sandbox_workflows_write_enabled,
        ),
    ] {
        if let Some(Some(value)) = value {
            *stored = value;
        }
    }
}

/// Role ids to assign by default; an unknown oid is a 422 so a stale form
/// cannot silently drop a selection.
async fn resolve_default_role_ids(
    pool: &sqlx::PgPool,
    oids: &[String],
) -> Result<Vec<i64>, ConsoleError> {
    let decoded = oids
        .iter()
        .map(|oid| RoleModel::decode_oid(oid))
        .collect::<Vec<_>>();
    let requested = decoded.iter().flatten().copied().collect::<Vec<_>>();
    let found = existing_role_ids(pool, &requested).await?;
    let missing = oids
        .iter()
        .zip(&decoded)
        .filter(|(_, id)| id.is_none_or(|id| !found.contains(&id)))
        .map(|(oid, _)| oid.clone())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(common::field_error(
            "default_role_ids",
            format!("contains unknown roles: {}", missing.join(", ")),
        ));
    }
    Ok(found)
}

/// Trim each id, drop blanks, and deduplicate.
fn default_role_oids(role_ids: Vec<String>) -> Vec<String> {
    let mut oids: Vec<String> = Vec::new();
    for oid in role_ids {
        let oid = oid.trim().to_owned();
        if !oid.is_empty() && !oids.contains(&oid) {
            oids.push(oid);
        }
    }
    oids
}

async fn settings_payload(pool: &sqlx::PgPool) -> Result<SettingsData, ConsoleError> {
    let setting = system_setting(pool).await?;
    let roles = roles(pool).await?;
    Ok(SettingsData {
        default_role_ids: roles
            .iter()
            .filter(|role| role.assign_by_default)
            .map(|role| RoleModel::oid(role.id))
            .collect(),
        roles: roles.iter().map(SettingsRole::from).collect(),
        values: setting.values,
        updated_at: setting.updated_at,
    })
}

async fn slack_external_teams(
    State(state): State<AppState>,
    _user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    let unavailable =
        || ConsoleError::new(StatusCode::SERVICE_UNAVAILABLE, CONSOLE_LOAD_UNAVAILABLE);
    let pool = session_db(&state).map_err(|()| unavailable())?;
    match PgSessionStore::new(pool).slack_external_team_ids().await {
        Ok(ids) => Ok(json_data(SlackExternalTeamIds {
            slack_external_team_ids: ids,
        })),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "console_slack_external_team_ids_load_failed"
            );
            Err(unavailable())
        }
    }
}

async fn replace_slack_external_teams(
    State(state): State<AppState>,
    _user: ConsoleUser,
    ConsoleData(data): ConsoleData<ReplaceSlackExternalTeams>,
) -> Result<Response, ConsoleError> {
    let unavailable =
        || ConsoleError::new(StatusCode::SERVICE_UNAVAILABLE, CONSOLE_SAVE_UNAVAILABLE);
    let pool = session_db(&state).map_err(|()| unavailable())?;
    // api-rs rejects bad ids with a 400 whose message Console shows as a 422,
    // and any other failure is the documented unavailable error.
    let ids = match normalize_slack_external_team_ids(&requested_team_ids(
        data.slack_external_team_ids,
    )) {
        Ok(ids) => ids,
        Err(crate::ApiError::BadRequest(message)) => {
            return Err(ConsoleError::unprocessable(message));
        }
        Err(error) => {
            tracing::warn!(error = %error, "console_slack_external_team_ids_save_failed");
            return Err(unavailable());
        }
    };
    PgSessionStore::new(pool)
        .replace_slack_external_team_ids(&ids)
        .await
        .map_err(|error| {
            tracing::warn!(error = %error, "console_slack_external_team_ids_save_failed");
            unavailable()
        })?;
    Ok(json_data(SlackExternalTeamIds {
        slack_external_team_ids: ids,
    }))
}

/// A list, or one string split on whitespace and commas (the textarea form).
fn requested_team_ids(input: Option<SlackTeamIdInput>) -> Vec<String> {
    let list = match input {
        Some(SlackTeamIdInput::List(items)) => items,
        Some(SlackTeamIdInput::Text(text)) => text
            .split(|character: char| character.is_whitespace() || character == ',')
            .map(str::to_owned)
            .collect(),
        None => Vec::new(),
    };
    list.into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect()
}

/// The allowlist lives in api-rs's own database (the session store), not in
/// Console's, and is served from `AppState::pool()` (see
/// `crate::slack_external_teams`).
fn session_db(state: &AppState) -> Result<sqlx::PgPool, ()> {
    state.pool().map_err(|error| {
        tracing::warn!(error = %error, "console_slack_external_team_ids_store_unavailable");
    })
}

const CONSOLE_LOAD_UNAVAILABLE: &str =
    "The allowed external workspaces could not be loaded from the Centaur API.";
const CONSOLE_SAVE_UNAVAILABLE: &str =
    "Slack Connect workspaces could not be saved because the Centaur API is unavailable.";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The `details` keys a settings write is rejected under, or `None` when it
    /// passes.
    fn rejected(body: serde_json::Value) -> Option<Vec<String>> {
        common::detail_keys::<UpdateSystemSettings>(body)
    }

    #[test]
    fn the_repo_cache_default_is_included_in_the_supported_values() {
        for repo_cache in [json!("bogus"), json!(null)] {
            assert_eq!(
                rejected(json!({ "default_sandbox_repo_cache": repo_cache })),
                Some(vec!["default_sandbox_repo_cache".to_owned()])
            );
        }
        assert_eq!(
            rejected(json!({ "default_sandbox_repo_cache": "public" })),
            None
        );
        assert_eq!(rejected(json!({})), None);
    }

    #[test]
    fn a_null_boolean_is_out_of_the_list() {
        assert_eq!(
            rejected(json!({ "default_sandbox_workflows_read_enabled": null })),
            Some(vec!["default_sandbox_workflows_read_enabled".to_owned()])
        );
        assert_eq!(
            rejected(json!({ "default_sandbox_workflows_read_enabled": true })),
            None
        );
    }

    #[test]
    fn every_failing_setting_is_reported() {
        assert_eq!(
            rejected(json!({
                "default_sandbox_repo_cache": "bogus",
                "default_sandbox_observability_enabled": null,
            })),
            Some(vec![
                "default_sandbox_observability_enabled".to_owned(),
                "default_sandbox_repo_cache".to_owned(),
            ])
        );
    }

    #[test]
    fn a_scalar_setting_does_not_take_a_nested_object() {
        assert!(
            serde_json::from_value::<UpdateSystemSettings>(
                json!({ "default_sandbox_repo_cache": { "nested": true } })
            )
            .is_err()
        );
    }
}
