//! SSH keys granted to the proxy's principal, released per connection for
//! destinations the deployment allows.
use axum::{Json, extract::State, http::HeaderMap};
use centaur_console_db::{SecretValues, SshKey, ssh_keys};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{ApiError, bearer_token, database::load_proxy, models::AppState};

/// Longest lease a grant carries. Iron renews every five seconds, so revoking a
/// grant or replacing a key ends active connections well within this window.
const LEASE_SECONDS: i64 = 30;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    route: String,
    public_key: String,
    destination: String,
}

/// Iron's SSH authorizer. The principal comes only from the proxy token's
/// current assignment; the key is whichever SSH key that principal is granted,
/// directly or through a role, exactly like any other secret. A channel or
/// workflow can therefore use a key only if an admin granted it one.
pub(crate) async fn authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<Request>,
) -> Result<Json<Value>, ApiError> {
    let token = bearer_token(&headers).ok_or_else(ApiError::unauthorized)?;
    let proxy = load_proxy(&state.pool, token)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    let principal_id = proxy.principal_id.ok_or_else(ApiError::unauthorized)?;
    let assigned_at = proxy
        .principal_assigned_at
        .ok_or_else(ApiError::unauthorized)?;
    if request.route != "ee"
        || request.public_key.len() > 8192
        || !request.public_key.starts_with("ssh-ed25519 ")
    {
        return Err(ApiError::unauthorized());
    }
    let (destination, policy) = target_policy(&state.ssh_targets, &request.destination)?;
    let keys = ssh_keys(
        &state.pool,
        SecretValues::Live(&state.encryption),
        principal_id,
    )
    .await?;
    let key = strongest_key(&keys).ok_or_else(ApiError::unauthorized)?;
    let lease_expiry = Utc::now() + chrono::Duration::seconds(LEASE_SECONDS);
    Ok(Json(json!({
        "id": centaur_console_db::oid("sks", key.id),
        "principal": centaur_console_db::oid("prn", principal_id),
        // Reassignment, including away and back, and key replacement both
        // change the generation, which closes connections using the old one.
        "generation": format!(
            "{}:{}:{}:{}",
            key.id,
            key.version,
            proxy.id,
            assigned_at.to_rfc3339()
        ),
        "destination": destination,
        "username": policy.username,
        "host_key": "",
        "tls_identity": true,
        "private_key": key.private_key,
        "expires_at": lease_expiry.to_rfc3339(),
    })))
}

/// The key with the strictly highest grant priority. Equal top priorities are
/// ambiguous and deny, rather than picking one by an incidental order.
fn strongest_key(keys: &[SshKey]) -> Option<&SshKey> {
    let first = keys.first()?;
    match keys.get(1) {
        Some(second) if second.priority == first.priority => None,
        _ => Some(first),
    }
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

    fn key(id: i64, priority: i32) -> SshKey {
        SshKey {
            id,
            priority,
            version: String::new(),
            private_key: Value::Null,
        }
    }

    #[test]
    fn strongest_key_requires_a_unique_top_priority() {
        assert!(strongest_key(&[]).is_none());
        assert_eq!(strongest_key(&[key(1, 0)]).unwrap().id, 1);
        assert_eq!(strongest_key(&[key(2, 100), key(1, 0)]).unwrap().id, 2);
        assert!(strongest_key(&[key(2, 100), key(1, 100)]).is_none());
    }
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
