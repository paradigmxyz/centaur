//! The single sandbox token proxy-sync mints per proxy.
//!
//! One `Authorization: Bearer` injection, host-only for every api-rs host
//! (including the Console host), replaces the old `centaur-api` principal JWT
//! and the sandbox entitlement JWT. iron-proxy injects it once, so there is no
//! overlapping injection and no reliance on rule order.
//!
//! The token carries the principal's sandbox capability flags, its Slack
//! channel lists, the four sandbox API capabilities, and `console.delegate`
//! when the principal is a Slack DM in the bot's home workspace linked to an
//! active Console admin. api-rs still re-checks the proxy -> principal binding
//! and the delegated-admin rule on every request.

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use serde_json::{Value, json};

use centaur_console_db::{models::Config, oid, slack_channels};

use crate::{
    ApiError,
    models::{AppState, ProxyRecord},
};

pub(crate) const SANDBOX_TOKEN_AUDIENCE: &str = "centaur-sandbox";
pub(crate) const SANDBOX_TOKEN_ISSUER: &str = "centaur-console";
/// Capability names are shared with `centaur-api-server`; the token claim is
/// the wire contract between the two processes.
const SANDBOX_API_CAPABILITIES: [&str; 5] = [
    "slack.proxy",
    "sandbox.permissions",
    "sandbox.skills",
    "sandbox.tasks",
    "sandbox.oauth_apps",
];

#[derive(Serialize)]
struct SandboxTokenClaims {
    iss: &'static str,
    aud: &'static str,
    iat: i64,
    exp: i64,
    sub: String,
    sandbox_id: String,
    proxy_id: String,
    capabilities: Vec<&'static str>,
    slack: SlackChannels,
    console_delegate: bool,
}

#[derive(Serialize)]
struct SlackChannels {
    upload_channels: Vec<String>,
    download_channels: Vec<String>,
    history_channels: Vec<String>,
}

/// The rotation windows a synced config hash depends on.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct TokenWindows {
    pub(crate) sandbox: Option<i64>,
}

pub(crate) fn token_windows(state: &AppState, proxy: &ProxyRecord, now: i64) -> TokenWindows {
    let sandbox = proxy
        .principal_id
        .filter(|_| state.jwt_secret.is_some() && !state.api_hosts.is_empty())
        .map(|principal_id| window_start(&oid("prn", principal_id), now, 86_400));
    TokenWindows { sandbox }
}

pub(crate) async fn append_sandbox_token(
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
    let channels = slack_channels(&state.pool, principal_id).await?;
    let principal_oid = oid("prn", principal_id);
    let iat = window_start(&principal_oid, now, 86_400);

    let mut capabilities = Vec::new();
    if proxy
        .principal_field("sandbox_sessions_read_enabled")
        .as_bool()
        .unwrap_or(false)
    {
        capabilities.push("sessions.read");
    }
    if proxy
        .principal_field("sandbox_workflows_read_enabled")
        .as_bool()
        .unwrap_or(false)
    {
        capabilities.push("workflows.read");
    }
    if proxy
        .principal_field("sandbox_workflows_write_enabled")
        .as_bool()
        .unwrap_or(false)
    {
        capabilities.push("workflows.write");
    }
    capabilities.extend(SANDBOX_API_CAPABILITIES);
    if proxy.console_delegate() {
        capabilities.push("console.delegate");
    }

    let claims = SandboxTokenClaims {
        iss: SANDBOX_TOKEN_ISSUER,
        aud: SANDBOX_TOKEN_AUDIENCE,
        iat,
        exp: iat + 259_200,
        sub: principal_oid,
        sandbox_id: proxy.name.clone(),
        proxy_id: oid("prx", proxy.id),
        capabilities,
        slack: SlackChannels {
            upload_channels: channels.upload,
            download_channels: channels.download,
            history_channels: channels.history,
        },
        console_delegate: proxy.console_delegate(),
    };
    let value = jwt(&claims, secret)?;
    config.secrets.push(json!({
        "source": { "type": "control_plane", "value": value },
        "inject": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
        "rules": host_rules(&state.api_hosts),
    }));
    Ok(())
}

/// Host-only rules: the sandbox token is the default credential for every
/// api-rs path on every api-rs host, including the Console host.
fn host_rules(hosts: &[String]) -> Vec<Value> {
    hosts.iter().map(|host| json!({ "host": host })).collect()
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{SANDBOX_API_CAPABILITIES, host_rules, window_start};

    #[test]
    fn windowed_tokens_are_stable_within_a_window() {
        let start = window_start("prn_example", 1_700_000_001, 86_400);
        assert_eq!(start, window_start("prn_example", start + 86_399, 86_400));
    }

    #[test]
    fn the_single_token_rule_is_host_only() {
        assert_eq!(
            host_rules(&["api.internal".to_owned(), "console.internal".to_owned()]),
            vec![
                json!({ "host": "api.internal" }),
                json!({ "host": "console.internal" })
            ]
        );
        // No path or method filter: one injection covers every api-rs route.
        assert_eq!(
            host_rules(&["api.internal".to_owned()])[0].get("paths"),
            None
        );
    }

    #[test]
    fn the_token_carries_every_sandbox_api_capability() {
        for capability in [
            "slack.proxy",
            "sandbox.permissions",
            "sandbox.skills",
            "sandbox.tasks",
            "sandbox.oauth_apps",
        ] {
            assert!(
                SANDBOX_API_CAPABILITIES.contains(&capability),
                "{capability}"
            );
        }
    }
}
