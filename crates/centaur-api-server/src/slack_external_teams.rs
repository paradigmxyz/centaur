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
pub(crate) fn normalize_slack_external_team_ids<S: AsRef<str>>(
    team_ids: &[S],
) -> Result<Vec<String>, ApiError> {
    let (valid, invalid) = partition_slack_external_team_ids(team_ids);
    if !invalid.is_empty() {
        return Err(ApiError::BadRequest(format!(
            "invalid Slack team ids: {}",
            invalid.join(", ")
        )));
    }
    if valid.len() > MAX_TEAM_IDS {
        return Err(ApiError::BadRequest(format!(
            "at most {MAX_TEAM_IDS} Slack team ids are allowed"
        )));
    }
    Ok(valid)
}

/// Splits ids into normalized valid ids and the invalid entries, skipping
/// blanks and duplicates.
pub fn partition_slack_external_team_ids<S: AsRef<str>>(
    team_ids: &[S],
) -> (Vec<String>, Vec<String>) {
    let mut valid: Vec<String> = Vec::new();
    let mut invalid = Vec::new();
    for team_id in team_ids {
        let team_id = team_id.as_ref().trim().to_ascii_uppercase();
        if team_id.is_empty() || valid.contains(&team_id) || invalid.contains(&team_id) {
            continue;
        }
        if is_slack_team_id(&team_id) {
            valid.push(team_id);
        } else {
            invalid.push(team_id);
        }
    }
    (valid, invalid)
}

fn is_slack_team_id(value: &str) -> bool {
    (2..=MAX_TEAM_ID_LEN).contains(&value.len())
        && value.starts_with(['T', 'E'])
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::{normalize_slack_external_team_ids, partition_slack_external_team_ids};

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

    #[test]
    fn partition_keeps_valid_ids_beside_invalid_ones() {
        assert_eq!(
            partition_slack_external_team_ids(&["T0PARTNER", "junk", "", "t0partner"]),
            (vec!["T0PARTNER".to_owned()], vec!["JUNK".to_owned()])
        );
    }
}
