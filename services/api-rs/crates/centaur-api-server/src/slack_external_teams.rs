//! Slack Connect workspaces Slackbot admits, managed from the Console.

use axum::{Json, extract::State};
use centaur_session_runtime::SessionRuntimeError;
use centaur_session_sqlx::PgSessionStore;
use serde::{Deserialize, Serialize};

use crate::{ApiError, routes::AppState};

const MAX_TEAM_IDS: usize = 1_000;
const MAX_TEAM_ID_LEN: usize = 32;

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SlackExternalTeamIds {
    slack_external_team_ids: Vec<String>,
}

pub(crate) async fn get_slack_external_team_ids(
    State(state): State<AppState>,
) -> Result<Json<SlackExternalTeamIds>, ApiError> {
    let slack_external_team_ids = PgSessionStore::new(state.pool()?)
        .slack_external_team_ids()
        .await
        .map_err(SessionRuntimeError::from)?;
    Ok(Json(SlackExternalTeamIds {
        slack_external_team_ids,
    }))
}

pub(crate) async fn replace_slack_external_team_ids(
    State(state): State<AppState>,
    Json(request): Json<SlackExternalTeamIds>,
) -> Result<Json<SlackExternalTeamIds>, ApiError> {
    let slack_external_team_ids =
        normalize_slack_external_team_ids(&request.slack_external_team_ids)?;
    PgSessionStore::new(state.pool()?)
        .replace_slack_external_team_ids(&slack_external_team_ids)
        .await
        .map_err(SessionRuntimeError::from)?;
    Ok(Json(SlackExternalTeamIds {
        slack_external_team_ids,
    }))
}

/// Trims, upper-cases, and de-duplicates Slack workspace (`T…`) and
/// Enterprise Grid org (`E…`) ids, rejecting anything else.
pub fn normalize_slack_external_team_ids<S: AsRef<str>>(
    team_ids: &[S],
) -> Result<Vec<String>, ApiError> {
    let mut normalized: Vec<String> = Vec::new();
    let mut invalid = Vec::new();
    for team_id in team_ids {
        let team_id = team_id.as_ref().trim().to_ascii_uppercase();
        if team_id.is_empty() || normalized.contains(&team_id) {
            continue;
        }
        if is_slack_team_id(&team_id) {
            normalized.push(team_id);
        } else {
            invalid.push(team_id);
        }
    }
    if !invalid.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "invalid Slack team ids: {}",
            invalid.join(", ")
        )));
    }
    if normalized.len() > MAX_TEAM_IDS {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_TEAM_IDS} Slack team ids are allowed"
        )));
    }
    Ok(normalized)
}

fn is_slack_team_id(value: &str) -> bool {
    (2..=MAX_TEAM_ID_LEN).contains(&value.len())
        && value.starts_with(['T', 'E'])
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::normalize_slack_external_team_ids;

    #[test]
    fn normalizes_and_validates_team_ids() {
        assert_eq!(
            normalize_slack_external_team_ids(&[" t0partner ", "E0GRID", "T0PARTNER", ""]).unwrap(),
            ["T0PARTNER", "E0GRID"]
        );
        let error = normalize_slack_external_team_ids(&["T0PARTNER", "C0CHANNEL", "T0-BAD"])
            .unwrap_err()
            .to_string();
        assert_eq!(error, "invalid Slack team ids: C0CHANNEL, T0-BAD");
    }
}
