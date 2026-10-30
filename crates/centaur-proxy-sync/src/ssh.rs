//! Personal SSH keys authorized against deployment-owned target policy.
use axum::{Json, extract::State, http::HeaderMap};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;

use crate::{ApiError, bearer_token, database::load_proxy, models::AppState};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    route: String,
    public_key: String,
    destination: String,
}

pub(crate) async fn authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Result<Json<Value>, ApiError> {
    let token = bearer_token(&headers).ok_or_else(ApiError::unauthorized)?;
    let proxy = load_proxy(&state.pool, token)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    let assigned_principal = proxy.principal_id.ok_or_else(ApiError::unauthorized)?;
    let assigned_at = proxy
        .principal_assigned_at
        .ok_or_else(ApiError::unauthorized)?;
    let principal_binding = format!("{}:{}", proxy.id, assigned_at.to_rfc3339());
    // The ingress derives this binding from the verified actor on each execute,
    // before the sandbox receives input. Never take a requester from the tool.
    let (principal_id, binding) = match proxy.principal_field("kind").as_str() {
        Some("console_user" | "slack_dm") => (assigned_principal, principal_binding),
        Some("slack_channel") => {
            let requester = proxy
                .requester_principal_id
                .ok_or_else(ApiError::unauthorized)?;
            let assigned_at = proxy
                .requester_principal_assigned_at
                .ok_or_else(ApiError::unauthorized)?;
            let personal: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM principals WHERE id = $1 AND kind = 'slack_dm' \
                 AND NULLIF(slack_user_id, '') IS NOT NULL AND NULLIF(slack_team_id, '') IS NOT NULL)",
            )
            .bind(requester)
            .fetch_one(&state.pool)
            .await
            .map_err(centaur_console_db::Error::Database)?;
            if !personal {
                return Err(ApiError::unauthorized());
            }
            (
                requester,
                format!("{principal_binding}:requester:{}", assigned_at.to_rfc3339()),
            )
        }
        _ => return Err(ApiError::unauthorized()),
    };
    if request.route != "ee"
        || request.public_key.len() > 8192
        || !request.public_key.starts_with("ssh-ed25519 ")
    {
        return Err(ApiError::unauthorized());
    }
    let (destination, policy) = target_policy(&state.ssh_targets, &request.destination)?;
    personal_grant(
        &state,
        principal_id,
        &binding,
        destination,
        &policy.username,
    )
    .await
}

// Resolve ownership through Console's authenticated account or its verified
// Slack SSO identity. Never match email, channel membership, or editable labels.
async fn personal_grant(
    state: &AppState,
    principal_id: i64,
    binding: &str,
    destination: String,
    username: &str,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query(
        "SELECT c.*, u.status AS owner_status FROM ssh_connections c \
         JOIN users u ON u.id = c.user_id JOIN principals p ON p.id = $1 \
         WHERE (p.kind = 'console_user' AND p.console_user_id = u.id) \
            OR (p.kind = 'slack_dm' AND EXISTS ( \
                SELECT 1 FROM user_identities i WHERE i.user_id = u.id AND i.provider = 'slack' \
                AND i.subject = p.slack_user_id AND i.team_id = p.slack_team_id \
                AND NULLIF(i.team_id, '') IS NOT NULL \
                AND (SELECT COUNT(DISTINCT (s.subject, s.team_id)) FROM user_identities s \
                     WHERE s.user_id = u.id AND s.provider = 'slack' \
                     AND NULLIF(s.subject, '') IS NOT NULL AND NULLIF(s.team_id, '') IS NOT NULL) = 1))",
    )
    .bind(principal_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(centaur_console_db::Error::Database)?;
    let row = row.ok_or_else(ApiError::unauthorized)?;
    let expires_at = row
        .get::<Option<chrono::NaiveDateTime>, _>("expires_at")
        .map(|expiry| expiry.and_utc());
    if row.get::<String, _>("owner_status") != "active"
        || row
            .get::<Option<chrono::NaiveDateTime>, _>("revoked_at")
            .is_some()
        || expires_at.is_some_and(|expiry| expiry <= Utc::now())
    {
        return Err(ApiError::unauthorized());
    }
    let encrypted: String = row
        .get::<Option<String>, _>("private_key")
        .ok_or_else(ApiError::unauthorized)?;
    let private_key = state
        .encryption
        .decrypt(&encrypted)
        .map_err(|_| ApiError::unauthorized())?;
    if private_key.is_empty() {
        return Err(ApiError::unauthorized());
    }
    let generation: String = row.get("generation");
    let lease_expiry = Utc::now() + chrono::Duration::seconds(30);
    Ok(Json(json!({
        "id": format!("ee:{generation}"),
        "principal": centaur_console_db::oid("prn", principal_id),
        // Reassignment, including away and back, invalidates the previous lease.
        "generation": format!("{generation}:{binding}"),
        "destination": destination,
        "username": username,
        "host_key": "",
        "tls_identity": true,
        "private_key": {"type": "control_plane", "value": private_key},
        "expires_at": expires_at.map_or(lease_expiry, |expiry| expiry.min(lease_expiry)).to_rfc3339(),
    })))
}

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TargetPolicy {
    host: String,
    path: String,
    username: String,
}

pub(crate) fn target_policies(raw: Option<&str>) -> Result<Vec<TargetPolicy>, String> {
    let policies: Vec<TargetPolicy> = serde_json::from_str(raw.unwrap_or("[]"))
        .map_err(|_| "CENTAUR_SSH_TARGETS must be a JSON array".to_owned())?;
    for p in &policies {
        let host = p.host.strip_prefix("*.").unwrap_or(&p.host);
        if host.is_empty()
            || host.starts_with('.')
            || host.ends_with('.')
            || !host
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-'))
            || !p.path.starts_with('/')
            || p.path.contains(['?', '#', '*'])
            || p.username.is_empty()
            || p.username.len() > 64
            || !p
                .username
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err("invalid SSH target policy: use an exact/lowercase wildcard host, exact path, and SSH username".to_owned());
        }
    }
    Ok(policies)
}

fn target_policy<'a>(
    policies: &'a [TargetPolicy],
    destination: &str,
) -> Result<(String, &'a TargetPolicy), ApiError> {
    if destination.len() > 2048 {
        return Err(ApiError::unauthorized());
    }
    let url = url::Url::parse(destination).map_err(|_| ApiError::unauthorized())?;
    if url.scheme() != "wss"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError::unauthorized());
    }
    let host = url.host_str().ok_or_else(ApiError::unauthorized)?;
    let mut matches = policies.iter().filter(|p| {
        let allowed = match p.host.strip_prefix("*.") {
            Some(suffix) => host
                .strip_suffix(suffix)
                .is_some_and(|prefix| prefix.ends_with('.') && prefix.len() > 1),
            None => host == p.host,
        };
        allowed && url.path() == p.path
    });
    let policy = matches.next().ok_or_else(ApiError::unauthorized)?;
    if matches.next().is_some() {
        return Err(ApiError::unauthorized());
    }
    Ok((url.to_string(), policy))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dynamic_targets_are_restricted_by_deployment_policy() {
        let policies = target_policies(Some(
            r#"[{"host":"*.dev.example.com","path":"/events/events","username":"dev"}]"#,
        ))
        .unwrap();
        for target in [
            "wss://one.dev.example.com/events/events",
            "wss://two.dev.example.com/events/events",
        ] {
            assert!(target_policy(&policies, target).is_ok());
        }
        for target in [
            "wss://dev.example.com/events/events",
            "wss://one.dev.example.com.evil.test/events/events",
            "wss://evildev.example.com/events/events",
            "wss://one.dev.example.com/other",
            "wss://one.dev.example.com:8443/events/events",
            "wss://user:secret@one.dev.example.com/events/events",
            "wss://one.dev.example.com/events/events?token=secret",
            "tcp://one.dev.example.com:22",
            "wss://169.254.169.254/events/events",
        ] {
            assert!(target_policy(&policies, target).is_err(), "{target}");
        }
        assert!(target_policy(&[], "wss://one.dev.example.com/events/events").is_err());
        let ambiguous = vec![policies[0].clone(), policies[0].clone()];
        assert!(target_policy(&ambiguous, "wss://one.dev.example.com/events/events").is_err());
        assert!(
            target_policies(Some(
                r#"[{"host":"*","path":"/events/events","username":"dev"}]"#
            ))
            .is_err()
        );
        assert!(target_policies(Some("not-json")).is_err());
    }
}
