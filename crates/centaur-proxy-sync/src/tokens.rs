//! The single sandbox token proxy-sync mints per proxy, and the short-lived
//! variant the tenancy iron-proxy uses for Code Mode.
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
//!
//! A Code Mode tenant has no proxy, so its token carries `binding:
//! "codemode"`, an empty `proxy_id`, and `sandbox_id: "codemode"`. It lives 15
//! minutes from a per-principal ten-minute window start, so the config hash
//! iron-proxy sees stays stable inside the window instead of changing on every
//! refresh.

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
/// The `sandbox_id` and `binding` claim a Code Mode tenant token carries in
/// place of a proxy.
pub(crate) const CODEMODE_SANDBOX_ID: &str = "codemode";
pub(crate) const CODEMODE_SYNC_BINDING: &str = "codemode";
/// The Code Mode tenant token's lifetime; api-rs accepts a `codemode` binding
/// for at most this long.
pub(crate) const CODEMODE_TOKEN_LIFETIME: i64 = 900;
/// How long a Code Mode token's signing window lasts: a config hash stays
/// stable for one window so iron-proxy is not rebuilt on every refresh.
pub(crate) const CODEMODE_TOKEN_WINDOW: i64 = 600;
/// Capability names are shared with `centaur-api-server`; the token claim is
/// the wire contract between the two processes.
const SANDBOX_API_CAPABILITIES: [&str; 5] = [
    "slack.proxy",
    "sandbox.permissions",
    "sandbox.skills",
    "sandbox.tasks",
    "sandbox.oauth_apps",
];
const PROXY_TOKEN_WINDOW: i64 = 86_400;
const PROXY_TOKEN_LIFETIME: i64 = 259_200;

#[derive(Serialize)]
struct SandboxTokenClaims {
    iss: &'static str,
    aud: &'static str,
    iat: i64,
    exp: i64,
    sub: String,
    sandbox_id: String,
    proxy_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    binding: Option<&'static str>,
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

/// The binding-specific identity fields of one sandbox token.
struct SandboxToken<'a> {
    sandbox_id: &'a str,
    proxy_id: String,
    binding: Option<&'static str>,
    iat: i64,
    exp: i64,
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
        .map(|principal_id| window_start(&oid("prn", principal_id), now, PROXY_TOKEN_WINDOW));
    TokenWindows { sandbox }
}

pub(crate) async fn append_sandbox_token(
    state: &AppState,
    proxy: &ProxyRecord,
    config: &mut Config,
    now: i64,
) -> Result<(), ApiError> {
    let Some(principal_id) = proxy.principal_id else {
        return Ok(());
    };
    let iat = window_start(&oid("prn", principal_id), now, PROXY_TOKEN_WINDOW);
    let token = SandboxToken {
        sandbox_id: &proxy.name,
        proxy_id: oid("prx", proxy.id),
        binding: None,
        iat,
        exp: iat + PROXY_TOKEN_LIFETIME,
    };
    if let Some(secret) = sandbox_secret(state, proxy, token).await? {
        config.secrets.push(secret);
    }
    Ok(())
}

/// The tenancy iron-proxy's api-rs credential: the same principal
/// configuration with a `codemode` binding, minted on a ten-minute window so
/// repeated refreshes hash identically.
pub(crate) async fn append_codemode_sandbox_token(
    state: &AppState,
    proxy: &ProxyRecord,
    config: &mut Config,
    now: i64,
) -> Result<(), ApiError> {
    let Some(principal_id) = proxy.principal_id else {
        return Ok(());
    };
    let iat = window_start(&oid("prn", principal_id), now, CODEMODE_TOKEN_WINDOW);
    let token = SandboxToken {
        sandbox_id: CODEMODE_SANDBOX_ID,
        proxy_id: String::new(),
        binding: Some(CODEMODE_SYNC_BINDING),
        iat,
        exp: iat + CODEMODE_TOKEN_LIFETIME,
    };
    if let Some(secret) = sandbox_secret(state, proxy, token).await? {
        config.secrets.push(secret);
    }
    Ok(())
}

/// Builds the injected token secret, or `None` when this deployment does not
/// mint sandbox tokens.
async fn sandbox_secret(
    state: &AppState,
    proxy: &ProxyRecord,
    token: SandboxToken<'_>,
) -> Result<Option<Value>, ApiError> {
    let (Some(secret), Some(principal_id)) = (&state.jwt_secret, proxy.principal_id) else {
        return Ok(None);
    };
    if state.api_hosts.is_empty() {
        return Ok(None);
    }
    let channels = slack_channels(&state.pool, principal_id).await?;
    let principal_oid = oid("prn", principal_id);

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
        iat: token.iat,
        exp: token.exp,
        sub: principal_oid,
        sandbox_id: token.sandbox_id.to_owned(),
        proxy_id: token.proxy_id,
        binding: token.binding,
        capabilities,
        slack: SlackChannels {
            upload_channels: channels.upload,
            download_channels: channels.download,
            history_channels: channels.history,
        },
        console_delegate: proxy.console_delegate(),
    };
    let value = jwt(&claims, secret)?;
    Ok(Some(json!({
        "source": { "type": "control_plane", "value": value },
        "inject": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
        "rules": host_rules(&state.api_hosts),
    })))
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

    use super::{
        CODEMODE_TOKEN_LIFETIME, CODEMODE_TOKEN_WINDOW, SANDBOX_API_CAPABILITIES, host_rules,
        window_start,
    };

    #[test]
    fn windowed_tokens_are_stable_within_a_window() {
        let start = window_start("prn_example", 1_700_000_001, 86_400);
        assert_eq!(start, window_start("prn_example", start + 86_399, 86_400));
    }

    #[test]
    fn codemode_tokens_rotate_on_their_own_window() {
        // The token outlives its window so a config served late in the window
        // still carries a usable credential.
        const _: () = assert!(CODEMODE_TOKEN_LIFETIME > CODEMODE_TOKEN_WINDOW);

        let start = window_start("prn_example", 1_700_000_123, CODEMODE_TOKEN_WINDOW);
        assert_eq!(
            start,
            window_start(
                "prn_example",
                start + CODEMODE_TOKEN_WINDOW - 1,
                CODEMODE_TOKEN_WINDOW
            )
        );
        assert_ne!(
            start,
            window_start(
                "prn_example",
                start + CODEMODE_TOKEN_WINDOW,
                CODEMODE_TOKEN_WINDOW
            )
        );
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
