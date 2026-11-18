//! What a sandbox may do: its effective permissions
//! (`GET /api/v1/sandbox/permissions`) and the OAuth apps its user can
//! connect (`GET /api/v1/sandbox/oauth_apps`).

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use centaur_console_db::{REDACTED, effective_config, oid};
use chrono::{DateTime, Utc};
use http::{HeaderMap, HeaderValue, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    ApiError, ApiResult, AppState,
    auth::{SandboxCaller, SandboxClaimsCaller, is_https},
    env::Env,
    extract::data,
};

/// The principal's configuration as proxy-sync serves it, with every inline
/// secret value redacted, plus the api-server token entry when the sandbox
/// may call it.
async fn redacted_config(state: &AppState, principal_id: i64) -> ApiResult<Value> {
    let config = effective_config(&state.db, principal_id)
        .await?
        .unwrap_or_default();
    let mut secrets = config.secrets;
    let hosts = api_server_hosts(&state.config.env);
    if !state.config.jwt_signing_secret.trim().is_empty() && !hosts.is_empty() {
        secrets.push(json!({
            "source": { "type": "control_plane", "value": REDACTED },
            "inject": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
            "rules": hosts.iter().map(|host| json!({ "host": host })).collect::<Vec<_>>(),
        }));
    }
    Ok(json!({
        "secrets": secrets,
        "transforms": config.transforms,
        "postgres": config.postgres,
    }))
}

/// Hosts of the api server: `CENTAUR_API_SERVER_PROXY_HOSTS` plus the host
/// of `CENTAUR_API_URL`, lower-cased and unique.
fn api_server_hosts(env: &Env) -> Vec<String> {
    let mut hosts: Vec<String> = env
        .var("CENTAUR_API_SERVER_PROXY_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(str::to_owned)
        .collect();
    if let Some(host) = env
        .var("CENTAUR_API_URL")
        .and_then(|url| url::Url::parse(url).ok())
        .and_then(|url| url.host_str().map(str::to_owned))
    {
        hosts.push(host);
    }
    let mut unique: Vec<String> = Vec::new();
    for host in hosts {
        let host = host.trim().trim_end_matches('.').to_lowercase();
        if !host.is_empty() && !unique.contains(&host) {
            unique.push(host);
        }
    }
    unique
}

#[derive(sqlx::FromRow)]
struct PrincipalRow {
    foreign_id: Option<String>,
    name: Option<String>,
    sandbox_repo_cache: String,
    sandbox_observability_enabled: bool,
    sandbox_sessions_read_enabled: bool,
    sandbox_workflows_read_enabled: bool,
    sandbox_workflows_write_enabled: bool,
}

#[derive(sqlx::FromRow)]
struct ChannelPermission {
    channel_id: String,
    upload_enabled: bool,
    download_enabled: bool,
    history_enabled: bool,
}

#[derive(sqlx::FromRow)]
struct OauthCredential {
    id: i64,
    app_id: i64,
    slug: String,
    provider: String,
    provider_email: Option<String>,
    provider_subject: Option<String>,
    dead: bool,
    last_refresh: Option<DateTime<Utc>>,
    scopes: Value,
}

/// Grants held directly or through a role.
const GRANTED: &str = "g.principal_id = $1 OR g.role_id IN \
    (SELECT role_id FROM principal_roles WHERE principal_id = $1)";

/// The grantable tables with their grant columns.
const GRANTABLES: [(&str, &str); 8] = [
    ("static_secrets", "static_secret_id"),
    ("gcp_auth_secrets", "gcp_auth_secret_id"),
    ("gcp_id_token_secrets", "gcp_id_token_secret_id"),
    ("aws_auth_secrets", "aws_auth_secret_id"),
    ("oauth_token_secrets", "oauth_token_secret_id"),
    ("pg_dsn_secrets", "pg_dsn_secret_id"),
    ("hmac_secrets", "hmac_secret_id"),
    ("ssh_key_secrets", "ssh_key_secret_id"),
];

pub async fn permissions(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
) -> ApiResult {
    let principal: PrincipalRow = sqlx::query_as(
        "SELECT foreign_id, name, sandbox_repo_cache, sandbox_observability_enabled, \
                sandbox_sessions_read_enabled, sandbox_workflows_read_enabled, \
                sandbox_workflows_write_enabled FROM principals WHERE id = $1",
    )
    .bind(auth.principal_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let channels: Vec<ChannelPermission> = sqlx::query_as(
        "SELECT upper(btrim(channel_id)) AS channel_id, bool_or(upload_enabled) AS upload_enabled, \
                bool_or(download_enabled) AS download_enabled, bool_or(history_enabled) AS history_enabled \
         FROM slack_channel_permissions \
         WHERE principal_id = $1 OR role_id IN (SELECT role_id FROM principal_roles WHERE principal_id = $1) \
         GROUP BY 1 ORDER BY 1",
    )
    .bind(auth.principal_id)
    .fetch_all(&state.db)
    .await?;
    let credentials: Vec<OauthCredential> = sqlx::query_as(&format!(
        "SELECT DISTINCT bc.id, app.id AS app_id, app.slug, app.provider, bc.provider_email, \
                bc.provider_subject, bc.dead, bc.last_refresh AT TIME ZONE 'UTC' AS last_refresh, \
                bc.scopes \
         FROM grants g JOIN static_secrets ss ON ss.id = g.static_secret_id \
         JOIN broker_credentials bc ON bc.id = ss.broker_credential_id \
         JOIN oauth_apps app ON app.id = bc.oauth_app_id \
         WHERE {GRANTED} ORDER BY app.slug, bc.provider_email NULLS FIRST, bc.id"
    ))
    .bind(auth.principal_id)
    .fetch_all(&state.db)
    .await?;
    let tools_sql = GRANTABLES
        .iter()
        .map(|(table, column)| {
            format!(
                "SELECT btrim(s.labels ->> 'centaur-tool') AS tool FROM grants g \
                 JOIN {table} s ON s.id = g.{column} \
                 WHERE ({GRANTED}) AND jsonb_typeof(s.labels -> 'centaur-tool') = 'string'"
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ");
    let tools: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT tool FROM ({tools_sql}) t WHERE tool <> '' ORDER BY tool"
    ))
    .bind(auth.principal_id)
    .fetch_all(&state.db)
    .await?;
    let principal_oid = oid("prn", auth.principal_id);
    let body = json!({ "data": {
        "sandbox_id": auth.proxy_name,
        "proxy_id": oid("prx", auth.proxy_id),
        "principal_id": principal_oid,
        "principal": {
            "id": principal_oid,
            "foreign_id": principal.foreign_id,
            "name": principal.name,
        },
        "capabilities": {
            "sandbox_repo_cache": principal.sandbox_repo_cache,
            "sandbox_observability_enabled": principal.sandbox_observability_enabled,
            "sandbox_sessions_read_enabled": principal.sandbox_sessions_read_enabled,
            "sandbox_workflows_read_enabled": principal.sandbox_workflows_read_enabled,
            "sandbox_workflows_write_enabled": principal.sandbox_workflows_write_enabled,
        },
        "slack_channel_permissions": channels.iter().map(|c| json!({
            "channel_id": c.channel_id,
            "upload_enabled": c.upload_enabled,
            "download_enabled": c.download_enabled,
            "history_enabled": c.history_enabled,
        })).collect::<Vec<_>>(),
        "oauth_credentials": credentials.iter().map(|c| json!({
            "id": oid("bcr", c.id),
            "oauth_app_id": oid("oap", c.app_id),
            "slug": c.slug,
            "provider": c.provider,
            "provider_email": c.provider_email,
            "provider_subject": c.provider_subject,
            "status": if c.dead { "dead" } else if c.last_refresh.is_none() { "bootstrapping" } else { "live" },
            "scopes": c.scopes,
        })).collect::<Vec<_>>(),
        "connected_tools": tools,
        "permissions": redacted_config(&state, auth.principal_id).await?,
    }});
    // A content-derived ETag lets clients detect changes.
    let etag = format!(
        "\"{}\"",
        hex::encode(Sha256::digest(body.to_string().as_bytes()))
    );
    let mut response: Response = axum::Json(body).into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(sqlx::FromRow)]
struct App {
    id: i64,
    slug: String,
    description: Option<String>,
    labels: Value,
    provider: String,
    allowed_scopes: Value,
}

/// The browser-visible base URL: `PUBLIC_URL`, else the request's own.
fn base_url(state: &AppState, headers: &HeaderMap) -> String {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    let scheme = if is_https(state, headers) {
        "https"
    } else {
        "http"
    };
    state.public_base_url(&format!("{scheme}://{host}"))
}

/// Enabled, non-shared OAuth apps and where a user starts consent.
pub async fn oauth_apps(
    State(state): State<AppState>,
    SandboxClaimsCaller(_claims): SandboxClaimsCaller,
    headers: HeaderMap,
) -> ApiResult {
    let apps: Vec<App> = sqlx::query_as(
        "SELECT id, slug, description, labels, provider, allowed_scopes FROM oauth_apps \
         WHERE enabled AND NOT shared ORDER BY slug ASC, id ASC",
    )
    .fetch_all(&state.db)
    .await?;
    let base = base_url(&state, &headers);
    let body: Vec<Value> = apps
        .iter()
        .map(|app| {
            json!({
                "id": oid("oap", app.id),
                "slug": app.slug,
                "description": app.description,
                "labels": app.labels,
                "provider": app.provider,
                "allowed_scopes": app.allowed_scopes,
                "start_url": format!("{base}/oauth/{}/start", app.slug),
            })
        })
        .collect();
    Ok(data(body).into_response())
}
