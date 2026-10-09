use std::env;

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use serde_json::json;

use crate::{
    ApiError,
    database::permission_channels,
    identifiers::oid,
    models::{AppState, Config, ProxyRecord},
};

#[derive(Serialize)]
struct ApiJwtClaims {
    iss: String,
    aud: String,
    iat: i64,
    exp: i64,
    sub: String,
    capabilities: Capabilities,
    slack: SlackChannels,
}

#[derive(Serialize)]
struct Capabilities {
    sessions_read: bool,
    workflows_read: bool,
    workflows_write: bool,
}

#[derive(Serialize)]
struct SlackChannels {
    upload_channels: Vec<String>,
    download_channels: Vec<String>,
    history_channels: Vec<String>,
}

#[derive(Serialize)]
struct SandboxJwtClaims {
    iss: String,
    aud: String,
    iat: i64,
    exp: i64,
    sub: String,
    sandbox_id: String,
    proxy_id: String,
    principal_id: String,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct TokenWindows {
    pub(crate) api: Option<i64>,
    pub(crate) sandbox: Option<i64>,
}

pub(crate) fn token_windows(state: &AppState, proxy: &ProxyRecord, now: i64) -> TokenWindows {
    let api = proxy
        .principal_id
        .filter(|_| state.jwt_secret.is_some() && !state.api_hosts.is_empty())
        .map(|principal_id| window_start(&oid("prn", principal_id), now, 900));
    let sandbox = proxy
        .principal_id
        .filter(|_| state.jwt_secret.is_some() && state.console_host.is_some())
        .map(|_| window_start(&oid("prx", proxy.id), now, 86_400));
    TokenWindows { api, sandbox }
}

pub(crate) async fn append_api_jwt(
    state: &AppState,
    proxy: &ProxyRecord,
    config: &mut Config,
    now: i64,
) -> Result<(), ApiError> {
    let (Some(secret), Some(principal_id)) = (&state.jwt_secret, proxy.principal_id) else {
        return Ok(());
    };
    if state.api_hosts.is_empty() {
        return Ok(());
    }
    let (upload_channels, download_channels, history_channels) =
        permission_channels(&state.pool, principal_id).await?;
    let principal_oid = oid("prn", principal_id);
    let iat = window_start(&principal_oid, now, 900);
    let claims = ApiJwtClaims {
        iss: env_default("CENTAUR_API_JWT_ISSUER", "centaur-console"),
        aud: env_default("CENTAUR_API_JWT_AUDIENCE", "centaur-api"),
        iat,
        exp: iat + 3600,
        sub: principal_oid,
        capabilities: Capabilities {
            sessions_read: proxy
                .principal_field("sandbox_sessions_read_enabled")
                .as_bool()
                .unwrap_or(false),
            workflows_read: proxy
                .principal_field("sandbox_workflows_read_enabled")
                .as_bool()
                .unwrap_or(false),
            workflows_write: proxy
                .principal_field("sandbox_workflows_write_enabled")
                .as_bool()
                .unwrap_or(false),
        },
        slack: SlackChannels {
            upload_channels,
            download_channels,
            history_channels,
        },
    };
    let token = jwt(&claims, secret)?;
    config.secrets.push(json!({
        "source": { "type": "control_plane", "value": token },
        "inject": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
        "rules": state.api_hosts.iter().map(|host| json!({ "host": host })).collect::<Vec<_>>()
    }));
    Ok(())
}

pub(crate) fn append_sandbox_jwt(
    state: &AppState,
    proxy: &ProxyRecord,
    config: &mut Config,
    now: i64,
) -> Result<(), ApiError> {
    let (Some(secret), Some(host), Some(principal_id)) =
        (&state.jwt_secret, &state.console_host, proxy.principal_id)
    else {
        return Ok(());
    };
    let proxy_oid = oid("prx", proxy.id);
    let iat = window_start(&proxy_oid, now, 86_400);
    let claims = SandboxJwtClaims {
        iss: env_default("CENTAUR_SANDBOX_ENTITLEMENTS_JWT_ISSUER", "centaur-console"),
        aud: env_default(
            "CENTAUR_SANDBOX_ENTITLEMENTS_JWT_AUDIENCE",
            "centaur-console-sandbox-entitlements",
        ),
        iat,
        exp: iat + 259_200,
        sub: proxy.name.clone(),
        sandbox_id: proxy.name.clone(),
        proxy_id: proxy_oid,
        principal_id: oid("prn", principal_id),
    };
    config.secrets.push(json!({
        "source": { "type": "control_plane", "value": jwt(&claims, secret)? },
        "inject": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
        "rules": [{ "host": host, "methods": ["GET", "POST", "PUT", "PATCH", "DELETE"], "paths": ["/api/v1/sandbox/*"] }]
    }));
    Ok(())
}

fn jwt<T: Serialize>(claims: &T, secret: &str) -> Result<String, ApiError> {
    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_owned());
    encode(
        &header,
        claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|_| ApiError::internal("JWT encoding failed"))
}

fn window_start(subject: &str, timestamp: i64, window: i64) -> i64 {
    let offset = (crc32fast::hash(subject.as_bytes()) as i64) % window;
    timestamp - (timestamp - offset).rem_euclid(window)
}

fn env_default(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

#[cfg(test)]
mod tests {
    use super::window_start;

    #[test]
    fn windowed_tokens_are_stable_within_a_window() {
        let start = window_start("prn_example", 1_700_000_001, 900);
        assert_eq!(start, window_start("prn_example", start + 899, 900));
    }
}
