use std::env;

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use serde_json::{Value, json};

use centaur_console_db::{models::Config, oid, slack_channels};

use crate::{
    ApiError,
    models::{AppState, ProxyRecord},
};

#[derive(Serialize)]
struct ApiJwtClaims {
    iss: &'static str,
    aud: &'static str,
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

/// Console admin routes (the `centaur_console` connector) use the same
/// sandbox-delegated credential as the sandbox API. iron-proxy applies matching
/// inject secrets in array order and the last write wins, so `build_config`
/// appends this entry after the API JWT: on these paths the entitlement token
/// is the `Authorization` value, while api-rs's own routes keep the API JWT.
const SANDBOX_JWT_METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];
const SANDBOX_JWT_PATHS: [&str; 2] = ["/api/v1/sandbox/*", "/api/v1/console/*"];

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
    let channels = slack_channels(&state.pool, principal_id).await?;
    let principal_oid = oid("prn", principal_id);
    let iat = window_start(&principal_oid, now, 900);
    let claims = ApiJwtClaims {
        iss: "centaur-console",
        aud: "centaur-api",
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
            upload_channels: channels.upload,
            download_channels: channels.download,
            history_channels: channels.history,
        },
    };
    let token = jwt(&claims, secret)?;
    config.secrets.push(json!({
        "source": { "type": "control_plane", "value": token },
        "inject": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
        "rules": api_jwt_rules(&state.api_hosts)
    }));
    Ok(())
}

/// Host-only rules: the API JWT is the default credential for every api-rs path
/// on the api hosts.
fn api_jwt_rules(hosts: &[String]) -> Vec<Value> {
    hosts.iter().map(|host| json!({ "host": host })).collect()
}

fn sandbox_jwt_rules(host: &str) -> Vec<Value> {
    vec![json!({
        "host": host,
        "methods": SANDBOX_JWT_METHODS,
        "paths": SANDBOX_JWT_PATHS,
    })]
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
        "rules": sandbox_jwt_rules(host)
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
    use serde_json::json;

    use super::{api_jwt_rules, sandbox_jwt_rules, window_start};

    #[test]
    fn windowed_tokens_are_stable_within_a_window() {
        let start = window_start("prn_example", 1_700_000_001, 900);
        assert_eq!(start, window_start("prn_example", start + 899, 900));
    }

    #[test]
    fn sandbox_token_covers_sandbox_and_console_paths() {
        assert_eq!(
            sandbox_jwt_rules("console.internal"),
            vec![json!({
                "host": "console.internal",
                "methods": ["GET", "POST", "PUT", "PATCH", "DELETE"],
                "paths": ["/api/v1/sandbox/*", "/api/v1/console/*"],
            })]
        );
    }

    #[test]
    fn console_paths_match_both_rules_and_the_sandbox_rule_is_appended_last() {
        // build_config appends the API JWT first and the sandbox JWT second.
        // iron-proxy applies every matching inject rule in array order and the
        // last write wins, so the entitlement token is the `Authorization`
        // value on Console paths. The API rule has no path filter, so api-rs's
        // own routes on the same host still receive the API JWT.
        let mut rules = api_jwt_rules(&["console.internal".to_owned()]);
        rules.extend(sandbox_jwt_rules("console.internal"));
        assert_eq!(rules[0], json!({ "host": "console.internal" }));
        assert_eq!(
            rules[1]["paths"],
            json!(["/api/v1/sandbox/*", "/api/v1/console/*"])
        );
        assert!(matches_path(&rules[0], "/api/v1/console/roles"));
        assert!(matches_path(&rules[1], "/api/v1/console/roles"));
        assert!(matches_path(
            &rules[1],
            "/api/v1/console/principals/prn_example"
        ));
        assert!(matches_path(&rules[1], "/api/v1/console"));
        assert!(!matches_path(&rules[1], "/api/session/example"));
    }

    /// iron-proxy's `hostmatch.MatchPath`: a trailing `/*` matches the prefix
    /// and the base path (including nested paths); an absent `paths` key
    /// matches every path.
    fn matches_path(rule: &serde_json::Value, path: &str) -> bool {
        let Some(paths) = rule.get("paths").and_then(serde_json::Value::as_array) else {
            return true;
        };
        paths.iter().any(|pattern| {
            let Some(pattern) = pattern.as_str() else {
                return false;
            };
            if let Some(base) = pattern.strip_suffix("/*") {
                path == base || path.starts_with(&format!("{base}/"))
            } else {
                false
            }
        })
    }
}
