mod active_record_encryption;

use std::{collections::HashMap, env, net::SocketAddr, sync::Arc};

use active_record_encryption::{ActiveRecordEncryption, Error as EncryptionError};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use url::Url;

const CREDENTIALS_SQL: &str = include_str!("../sql/effective_credentials.sql");
const EMPTY_ARRAY: Value = Value::Array(Vec::new());

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    encryption: Arc<ActiveRecordEncryption>,
    jwt_secret: Option<String>,
    api_hosts: Vec<String>,
    console_host: Option<String>,
}

#[derive(Debug)]
struct ProxyRecord {
    id: i64,
    name: String,
    labels: Value,
    principal_id: Option<i64>,
    requester_principal_id: Option<i64>,
    principal_assigned_at: Option<DateTime<Utc>>,
    requester_principal_assigned_at: Option<DateTime<Utc>>,
    principal: Option<Value>,
    console_user_email: Option<String>,
    console_user_id: Option<i64>,
    slack_history_channel_ids: Value,
}

#[derive(Debug)]
struct Credential {
    kind: String,
    id: i64,
    priority: i32,
    data: Value,
    sources: Vec<Value>,
    rules: Vec<Value>,
}

#[derive(Default)]
struct Config {
    secrets: Vec<Value>,
    transforms: Vec<Value>,
    postgres: Vec<Value>,
}

#[derive(Deserialize, Default)]
struct SyncRequest {
    config_hash: Option<String>,
}

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

#[derive(Serialize)]
struct ErrorEnvelope {
    error: ErrorMessage,
}

#[derive(Serialize)]
struct ErrorMessage {
    message: String,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "invalid or missing proxy token".to_owned(),
        }
    }
}

impl From<EncryptionError> for ApiError {
    fn from(error: EncryptionError) -> Self {
        Self::internal(error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            error!(error = %self.message, "proxy sync request failed");
        }
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorMessage {
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let database_url = required_env("IRON_CONTROL_DATABASE_URL")?;
    let primary_key = required_env("IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY")?;
    let salt = required_env("IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT")?;
    let pool = PgPoolOptions::new()
        .max_connections(
            env::var("DATABASE_MAX_CONNECTIONS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(20),
        )
        .connect(&database_url)
        .await?;
    let api_hosts = configured_hosts("CENTAUR_API_SERVER_PROXY_HOSTS", "CENTAUR_API_URL");
    let console_host = env::var("CENTAUR_CONSOLE_URL")
        .ok()
        .and_then(|url| host_from_url(&url));
    let state = AppState {
        pool,
        encryption: Arc::new(ActiveRecordEncryption::new(&primary_key, &salt)),
        jwt_secret: env::var("CENTAUR_JWT_SIGNING_SECRET")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        api_hosts,
        console_host,
    };
    let app = Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/api/v1/proxy/sync", post(sync))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let bind: SocketAddr = env::var("BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_owned())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    info!(%bind, "proxy sync service listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn sync(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SyncRequest>,
) -> Result<Json<Value>, ApiError> {
    let token = bearer_token(&headers).ok_or_else(ApiError::unauthorized)?;
    let proxy = load_proxy(&state.pool, token)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    let config = if let Some(principal_id) = proxy.principal_id {
        build_config(&state, &proxy, principal_id).await?
    } else {
        Config::default()
    };
    let config_value = json!({
        "secrets": config.secrets,
        "transforms": config.transforms,
        "postgres": config.postgres,
    });
    let config_hash = config_hash(&proxy, &config_value)?;
    if request.config_hash.as_deref().filter(|v| !v.is_empty()) == Some(config_hash.as_str()) {
        return Ok(Json(json!({ "config_hash": config_hash })));
    }
    Ok(Json(json!({
        "config_hash": config_hash,
        "status": if proxy.principal_id.is_some() { "assigned" } else { "unassigned" },
        "principal_id": proxy.principal_id.map(|id| oid("prn", id)),
        "secrets": config_value["secrets"],
        "transforms": config_value["transforms"],
        "postgres": config_value["postgres"],
    })))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|v| !v.is_empty())
}

async fn load_proxy(pool: &PgPool, token: &str) -> Result<Option<ProxyRecord>, ApiError> {
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let row = sqlx::query(
        "SELECT p.id, p.name, p.labels, p.principal_id, p.requester_principal_id, \
                p.principal_assigned_at AT TIME ZONE 'UTC' AS principal_assigned_at, \
                p.requester_principal_assigned_at AT TIME ZONE 'UTC' AS requester_principal_assigned_at, \
                to_jsonb(pr) AS principal, u.email AS console_user_email, u.id AS console_user_id, \
                COALESCE(( \
                    SELECT jsonb_agg(effective.channel_id ORDER BY effective.channel_id) \
                    FROM ( \
                        SELECT permissions.channel_id \
                        FROM slack_channel_permissions permissions \
                        WHERE permissions.principal_id = p.principal_id \
                           OR permissions.role_id IN (SELECT role_id FROM principal_roles WHERE principal_id = p.principal_id) \
                        GROUP BY permissions.channel_id \
                        HAVING bool_or(permissions.history_enabled) \
                    ) effective \
                ), '[]'::jsonb) AS slack_history_channel_ids \
         FROM proxies p \
         LEFT JOIN principals pr ON pr.id = p.principal_id \
         LEFT JOIN users u ON u.id = pr.console_user_id \
         WHERE p.bearer_token_hash = $1",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await
    .map_err(db_error)?;
    Ok(row.map(|row| ProxyRecord {
        id: row.get("id"),
        name: row.get("name"),
        labels: row.get("labels"),
        principal_id: row.get("principal_id"),
        requester_principal_id: row.get("requester_principal_id"),
        principal_assigned_at: row.get("principal_assigned_at"),
        requester_principal_assigned_at: row.get("requester_principal_assigned_at"),
        principal: row.get("principal"),
        console_user_email: row.get("console_user_email"),
        console_user_id: row.get("console_user_id"),
        slack_history_channel_ids: row.get("slack_history_channel_ids"),
    }))
}

async fn load_credentials(
    pool: &PgPool,
    principal_id: i64,
    requester_id: Option<i64>,
) -> Result<Vec<Credential>, ApiError> {
    let rows = sqlx::query(CREDENTIALS_SQL)
        .bind(principal_id)
        .bind(requester_id)
        .fetch_all(pool)
        .await
        .map_err(db_error)?;
    rows.into_iter()
        .map(|row| {
            Ok(Credential {
                kind: row.try_get("kind").map_err(db_error)?,
                id: row.try_get("credential_id").map_err(db_error)?,
                priority: row.try_get("effective_priority").map_err(db_error)?,
                data: row.try_get("credential").map_err(db_error)?,
                sources: json_array(row.try_get("sources").map_err(db_error)?),
                rules: json_array(row.try_get("rules").map_err(db_error)?),
            })
        })
        .collect()
}

async fn build_config(
    state: &AppState,
    proxy: &ProxyRecord,
    principal_id: i64,
) -> Result<Config, ApiError> {
    let loaded = load_credentials(&state.pool, principal_id, proxy.requester_principal_id).await?;
    let mut credentials = Vec::with_capacity(loaded.len());
    for credential in loaded {
        let deliverable = if credential.kind == "static" {
            match credential.sources.first() {
                Some(source) => source_value(source, &state.encryption)?.is_some(),
                None => false,
            }
        } else {
            true
        };
        if deliverable {
            credentials.push(credential);
        }
    }
    suppress_conflicts(&mut credentials);

    let mut config = Config::default();
    for credential in credentials.iter().filter(|c| c.kind == "static") {
        if let Some(source) = source_value(&credential.sources[0], &state.encryption)? {
            let mut entry = Map::new();
            entry.insert("source".to_owned(), source);
            entry.insert("rules".to_owned(), proxy_rules(&credential.rules));
            if present(credential.data.get("inject_config")) {
                entry.insert(
                    "inject".to_owned(),
                    credential.data["inject_config"].clone(),
                );
            }
            if present(credential.data.get("replace_config")) {
                entry.insert(
                    "replace".to_owned(),
                    credential.data["replace_config"].clone(),
                );
            }
            config.secrets.push(Value::Object(entry));
        }
    }
    append_api_jwt(state, proxy, &mut config).await?;
    append_sandbox_jwt(state, proxy, &mut config)?;

    for kind in ["gcp_auth", "gcp_id_token", "aws_auth", "hmac"] {
        for credential in credentials.iter().filter(|c| c.kind == kind) {
            config
                .transforms
                .push(transform(credential, &state.encryption)?);
        }
    }
    let oauth: Vec<Value> = credentials
        .iter()
        .filter(|c| c.kind == "oauth_token")
        .map(|c| oauth_entry(c, &state.encryption))
        .collect::<Result<_, _>>()?;
    if !oauth.is_empty() {
        config
            .transforms
            .push(json!({ "name": "oauth_token", "config": { "tokens": oauth } }));
    }
    append_postgres(proxy, &credentials, &state.encryption, &mut config)?;
    Ok(config)
}

fn source_value(
    source: &Value,
    encryption: &ActiveRecordEncryption,
) -> Result<Option<Value>, ApiError> {
    let source_type = string(source, "source_type");
    if source_type == "token_broker" {
        let Some(raw) = source.get("broker_access_token").and_then(Value::as_str) else {
            return Ok(None);
        };
        let value = encryption.decrypt(raw)?;
        if value.is_empty() {
            return Ok(None);
        }
        return Ok(Some(json!({ "type": "control_plane", "value": value })));
    }
    let mut result = source
        .get("config")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    result.insert("type".to_owned(), Value::String(source_type.to_owned()));
    if source_type == "control_plane" {
        let raw = source
            .get("secret")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::internal("control_plane source has no secret"))?;
        result.insert("value".to_owned(), Value::String(encryption.decrypt(raw)?));
    }
    Ok(Some(Value::Object(result)))
}

fn proxy_rules(rules: &[Value]) -> Value {
    Value::Array(
        rules
            .iter()
            .map(|rule| {
                let mut output = Map::new();
                for key in ["host", "cidr"] {
                    if let Some(value) = rule.get(key).filter(|v| present(Some(v))) {
                        output.insert(key.to_owned(), value.clone());
                    }
                }
                if let Some(value) = rule.get("http_methods").filter(|v| present(Some(v))) {
                    output.insert("methods".to_owned(), value.clone());
                }
                if let Some(value) = rule.get("paths").filter(|v| present(Some(v))) {
                    output.insert("paths".to_owned(), value.clone());
                }
                Value::Object(output)
            })
            .collect(),
    )
}

fn transform(c: &Credential, encryption: &ActiveRecordEncryption) -> Result<Value, ApiError> {
    let rules = proxy_rules(&c.rules);
    match c.kind.as_str() {
        "gcp_auth" => {
            let mut config = Map::new();
            if let Some(source) = c.sources.first()
                && let Some(value) = source_value(source, encryption)?
            {
                config.insert("keyfile".to_owned(), value);
            }
            copy_present(
                &c.data,
                &mut config,
                "credentials_provider",
                "credentials_provider",
            );
            copy_present(&c.data, &mut config, "subject", "subject");
            config.insert(
                "scopes".to_owned(),
                c.data
                    .get("scopes")
                    .cloned()
                    .unwrap_or_else(|| EMPTY_ARRAY.clone()),
            );
            config.insert("rules".to_owned(), rules);
            Ok(json!({ "name": "gcp_auth", "config": config }))
        }
        "gcp_id_token" => {
            let source = c
                .sources
                .first()
                .ok_or_else(|| ApiError::internal("gcp_id_token source missing"))?;
            let mut config = Map::new();
            config.insert(
                "keyfile".to_owned(),
                source_value(source, encryption)?
                    .ok_or_else(|| ApiError::internal("gcp_id_token source unavailable"))?,
            );
            config.insert("audience".to_owned(), c.data["audience"].clone());
            config.insert("rules".to_owned(), rules);
            copy_present(&c.data, &mut config, "header", "header");
            Ok(json!({ "name": "gcp_id_token", "config": config }))
        }
        "aws_auth" => {
            let mut config = Map::new();
            for source in &c.sources {
                if let Some(role) = source.get("role").and_then(Value::as_str)
                    && let Some(value) = source_value(source, encryption)?
                {
                    config.insert(role.to_owned(), value);
                }
            }
            copy_present(&c.data, &mut config, "allowed_regions", "allowed_regions");
            copy_present(&c.data, &mut config, "allowed_services", "allowed_services");
            config.insert("rules".to_owned(), rules);
            Ok(json!({ "name": "aws_auth", "config": config }))
        }
        "hmac" => {
            let mut credentials = Map::new();
            for source in &c.sources {
                if let Some(role) = source.get("role").and_then(Value::as_str)
                    && let Some(value) = source_value(source, encryption)?
                {
                    credentials.insert(role.to_owned(), value);
                }
            }
            let mut config = json!({
                "credentials": credentials,
                "timestamp": { "format": c.data["timestamp_format"] },
                "signature": {
                    "algorithm": c.data["signature_algorithm"],
                    "key_encoding": c.data["signature_key_encoding"],
                    "output_encoding": c.data["signature_output_encoding"],
                    "message": c.data["signature_message"]
                },
                "headers": c.data["headers"],
                "rules": rules
            });
            if c.data.get("allow_chunked_body").and_then(Value::as_bool) == Some(true) {
                config["allow_chunked_body"] = Value::Bool(true);
            }
            Ok(json!({ "name": "hmac_sign", "config": config }))
        }
        _ => Err(ApiError::internal("unknown transform kind")),
    }
}

fn oauth_entry(c: &Credential, encryption: &ActiveRecordEncryption) -> Result<Value, ApiError> {
    let mut entry = Map::new();
    entry.insert("grant".to_owned(), c.data["grant"].clone());
    entry.insert(
        "token_endpoint".to_owned(),
        c.data["token_endpoint"].clone(),
    );
    let mut headers = Map::new();
    for source in &c.sources {
        let Some(role) = source.get("role").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = source_value(source, encryption)? else {
            continue;
        };
        if string(source, "role_kind") == "endpoint_header" {
            headers.insert(role.to_owned(), value);
        } else {
            entry.insert(role.to_owned(), value);
        }
    }
    for key in ["audience", "scopes", "header", "value_prefix"] {
        copy_present(&c.data, &mut entry, key, key);
    }
    if !headers.is_empty() {
        entry.insert("token_endpoint_headers".to_owned(), Value::Object(headers));
    }
    entry.insert("rules".to_owned(), proxy_rules(&c.rules));
    Ok(Value::Object(entry))
}

fn append_postgres(
    proxy: &ProxyRecord,
    credentials: &[Credential],
    encryption: &ActiveRecordEncryption,
    config: &mut Config,
) -> Result<(), ApiError> {
    let mut positions: HashMap<String, usize> = HashMap::new();
    for c in credentials.iter().filter(|c| c.kind == "pg_dsn") {
        let Some(source) = c.sources.first() else {
            continue;
        };
        let Some(dsn) = source_value(source, encryption)? else {
            continue;
        };
        let database = string(&c.data, "database").to_owned();
        let mut entry = Map::new();
        entry.insert("id".to_owned(), Value::String(oid("pgs", c.id)));
        entry.insert("foreign_id".to_owned(), c.data["foreign_id"].clone());
        entry.insert("database".to_owned(), Value::String(database.clone()));
        entry.insert("dsn".to_owned(), dsn);
        copy_present(&c.data, &mut entry, "role", "role");
        let settings = postgres_settings(proxy, c.data.get("settings"));
        if !settings.is_empty() {
            entry.insert("settings".to_owned(), Value::Array(settings));
        }
        if let Some(index) = positions.get(&database).copied() {
            config.postgres[index] = Value::Object(entry);
        } else {
            positions.insert(database, config.postgres.len());
            config.postgres.push(Value::Object(entry));
        }
    }
    Ok(())
}

fn postgres_settings(proxy: &ProxyRecord, value: Option<&Value>) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|setting| {
            let name = setting.get("name")?.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            let value = if let Some(reference) =
                setting.get("value_from").and_then(Value::as_object)
            {
                if let Some(label) = reference.get("principal_label").and_then(Value::as_str) {
                    principal_field(proxy, "labels")
                        .get(label)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                } else if let Some(label) = reference.get("proxy_label").and_then(Value::as_str) {
                    proxy
                        .labels
                        .get(label)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                } else {
                    reference
                        .get("principal_field")
                        .and_then(Value::as_str)
                        .map(|field| principal_setting(proxy, field))
                        .unwrap_or_default()
                }
            } else {
                setting
                    .get("value")
                    .map(value_to_string)
                    .unwrap_or_default()
            };
            Some(json!({ "name": name, "value": value }))
        })
        .collect()
}

fn principal_setting(proxy: &ProxyRecord, field: &str) -> String {
    match field {
        "id" => proxy
            .principal_id
            .map(|id| oid("prn", id))
            .unwrap_or_default(),
        "console_user_id" => proxy
            .console_user_id
            .map(|id| oid("usr", id))
            .unwrap_or_default(),
        "console_user_email" => proxy.console_user_email.clone().unwrap_or_default(),
        "slack_history_channel_ids" => proxy.slack_history_channel_ids.to_string(),
        other => value_to_string(principal_field(proxy, other)),
    }
}

fn principal_field<'a>(proxy: &'a ProxyRecord, field: &str) -> &'a Value {
    proxy
        .principal
        .as_ref()
        .and_then(|p| p.get(field))
        .unwrap_or(&Value::Null)
}

async fn append_api_jwt(
    state: &AppState,
    proxy: &ProxyRecord,
    config: &mut Config,
) -> Result<(), ApiError> {
    let (Some(secret), Some(principal_id)) = (&state.jwt_secret, proxy.principal_id) else {
        return Ok(());
    };
    if state.api_hosts.is_empty() {
        return Ok(());
    }
    let permissions = permission_channels(&state.pool, principal_id).await?;
    let principal_oid = oid("prn", principal_id);
    let now = Utc::now().timestamp();
    let iat = window_start(&principal_oid, now, 900);
    let claims = ApiJwtClaims {
        iss: env_default("CENTAUR_API_JWT_ISSUER", "centaur-console"),
        aud: env_default("CENTAUR_API_JWT_AUDIENCE", "centaur-api"),
        iat,
        exp: iat + 3600,
        sub: principal_oid,
        capabilities: Capabilities {
            sessions_read: principal_field(proxy, "sandbox_sessions_read_enabled")
                .as_bool()
                .unwrap_or(false),
            workflows_read: principal_field(proxy, "sandbox_workflows_read_enabled")
                .as_bool()
                .unwrap_or(false),
            workflows_write: principal_field(proxy, "sandbox_workflows_write_enabled")
                .as_bool()
                .unwrap_or(false),
        },
        slack: permissions,
    };
    let token = jwt(&claims, secret)?;
    config.secrets.push(json!({
        "source": { "type": "control_plane", "value": token },
        "inject": { "header": "Authorization", "formatter": "Bearer {{ .Value }}" },
        "rules": state.api_hosts.iter().map(|host| json!({ "host": host })).collect::<Vec<_>>()
    }));
    Ok(())
}

async fn permission_channels(pool: &PgPool, principal_id: i64) -> Result<SlackChannels, ApiError> {
    let rows = sqlx::query(
        "SELECT channel_id, bool_or(upload_enabled) AS upload, bool_or(download_enabled) AS download, bool_or(history_enabled) AS history \
         FROM slack_channel_permissions \
         WHERE principal_id = $1 OR role_id IN (SELECT role_id FROM principal_roles WHERE principal_id = $1) \
         GROUP BY channel_id ORDER BY channel_id"
    ).bind(principal_id).fetch_all(pool).await.map_err(db_error)?;
    let mut upload = Vec::new();
    let mut download = Vec::new();
    let mut history = Vec::new();
    for row in rows {
        let channel: String = row.get("channel_id");
        if row.get("upload") {
            upload.push(channel.clone());
        }
        if row.get("download") {
            download.push(channel.clone());
        }
        if row.get("history") {
            history.push(channel);
        }
    }
    Ok(SlackChannels {
        upload_channels: upload,
        download_channels: download,
        history_channels: history,
    })
}

fn append_sandbox_jwt(
    state: &AppState,
    proxy: &ProxyRecord,
    config: &mut Config,
) -> Result<(), ApiError> {
    let (Some(secret), Some(host), Some(principal_id)) =
        (&state.jwt_secret, &state.console_host, proxy.principal_id)
    else {
        return Ok(());
    };
    let proxy_oid = oid("prx", proxy.id);
    let iat = window_start(&proxy_oid, Utc::now().timestamp(), 86_400);
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

fn suppress_conflicts(credentials: &mut Vec<Credential>) {
    let mut indexes: Vec<usize> = (0..credentials.len()).collect();
    indexes.sort_by_key(|&i| (-credentials[i].priority, -credentials[i].id));
    let mut claimed: HashMap<String, Vec<(String, i32)>> = HashMap::new();
    let mut suppressed = vec![false; credentials.len()];
    for i in indexes {
        let claims = conflict_claims(&credentials[i]);
        let stronger = claims.iter().any(|(scope, target)| {
            claimed.get(target).is_some_and(|prior| {
                prior.iter().any(|(other, priority)| {
                    *priority > credentials[i].priority && scopes_overlap(scope, other)
                })
            })
        });
        if stronger {
            suppressed[i] = true;
        } else {
            for (scope, target) in claims {
                claimed
                    .entry(target)
                    .or_default()
                    .push((scope, credentials[i].priority));
            }
        }
    }
    let mut index = 0;
    credentials.retain(|_| {
        let keep = !suppressed[index];
        index += 1;
        keep
    });
}

fn conflict_claims(c: &Credential) -> Vec<(String, String)> {
    if c.kind == "pg_dsn" {
        return Vec::new();
    }
    let targets: Vec<String> = match c.kind.as_str() {
        "static" => {
            if let Some(inject) = c.data.get("inject_config").filter(|v| present(Some(v))) {
                if let Some(header) = inject.get("header").and_then(Value::as_str) {
                    vec![format!("header:{}", header.to_lowercase())]
                } else if let Some(param) = inject.get("query_param").and_then(Value::as_str) {
                    vec![format!("query:{param}")]
                } else {
                    vec![]
                }
            } else {
                c.data
                    .get("replace_config")
                    .and_then(|v| v.get("match_headers"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|v| format!("header:{}", v.to_lowercase()))
                    .collect()
            }
        }
        "gcp_auth" | "aws_auth" | "oauth_token" => vec!["header:authorization".to_owned()],
        "gcp_id_token" => vec![format!(
            "header:{}",
            c.data
                .get("header")
                .and_then(Value::as_str)
                .unwrap_or("authorization")
        )],
        "hmac" => c
            .data
            .get("headers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|h| h.get("name").and_then(Value::as_str))
            .map(|h| format!("header:{}", h.to_lowercase()))
            .collect(),
        _ => vec![],
    };
    let scopes = c.rules.iter().filter_map(|rule| {
        if let Some(host) = rule.get("host").and_then(Value::as_str) {
            Some(format!(
                "host:{}",
                host.trim().trim_end_matches('.').to_lowercase()
            ))
        } else {
            rule.get("cidr")
                .and_then(Value::as_str)
                .map(|v| format!("cidr:{v}"))
        }
    });
    scopes
        .flat_map(|scope| {
            targets
                .iter()
                .cloned()
                .map(move |target| (scope.clone(), target))
        })
        .collect()
}

fn scopes_overlap(a: &str, b: &str) -> bool {
    let Some((kind_a, value_a)) = a.split_once(':') else {
        return false;
    };
    let Some((kind_b, value_b)) = b.split_once(':') else {
        return false;
    };
    if kind_a != kind_b {
        return false;
    }
    if kind_a == "cidr" {
        return value_a == value_b;
    }
    if value_a == value_b || value_a == "*" || value_b == "*" {
        return true;
    }
    let a: Vec<_> = value_a.split('.').collect();
    let b: Vec<_> = value_b.split('.').collect();
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| *x == "*" || y == "*" || *x == y)
}

fn config_hash(proxy: &ProxyRecord, config: &Value) -> Result<String, ApiError> {
    let mut payload = config
        .as_object()
        .cloned()
        .ok_or_else(|| ApiError::internal("config was not an object"))?;
    payload.insert(
        "principal".to_owned(),
        proxy
            .principal_id
            .map(|id| Value::String(oid("prn", id)))
            .unwrap_or(Value::Null),
    );
    payload.insert(
        "principal_assigned_at".to_owned(),
        timestamp(proxy.principal_assigned_at),
    );
    payload.insert("proxy_labels".to_owned(), proxy.labels.clone());
    if proxy.requester_principal_id.is_some() {
        payload.insert(
            "requester_principal".to_owned(),
            proxy
                .requester_principal_id
                .map(|id| Value::String(oid("prn", id)))
                .unwrap_or(Value::Null),
        );
        payload.insert(
            "requester_principal_assigned_at".to_owned(),
            timestamp(proxy.requester_principal_assigned_at),
        );
    }
    let canonical = serde_json::to_vec(&Value::Object(payload))
        .map_err(|_| ApiError::internal("config serialization failed"))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(canonical))))
}

fn oid(prefix: &str, id: i64) -> String {
    let mut alphabet: Vec<char> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        .chars()
        .collect();
    alphabet.sort_by_key(|c| hex::encode(Sha256::digest(format!("{prefix}:{c}").as_bytes())));
    let sqids = sqids::Sqids::builder()
        .alphabet(alphabet)
        .min_length(8)
        .build()
        .expect("valid sqids settings");
    format!(
        "{prefix}_{}",
        sqids.encode(&[id as u64]).expect("database IDs fit sqids")
    )
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

fn timestamp(value: Option<DateTime<Utc>>) -> Value {
    value
        .map(|v| Value::String(v.format("%Y-%m-%dT%H:%M:%SZ").to_string()))
        .unwrap_or(Value::Null)
}

fn configured_hosts(list_env: &str, url_env: &str) -> Vec<String> {
    let mut hosts: Vec<String> = env::var(list_env)
        .unwrap_or_default()
        .split(',')
        .map(str::to_owned)
        .collect();
    if let Ok(url) = env::var(url_env)
        && let Some(host) = host_from_url(&url)
    {
        hosts.push(host);
    }
    hosts
        .into_iter()
        .map(|h| h.trim().trim_end_matches('.').to_lowercase())
        .filter(|h| !h.is_empty())
        .fold(Vec::new(), |mut out, host| {
            if !out.contains(&host) {
                out.push(host);
            }
            out
        })
}

fn host_from_url(value: &str) -> Option<String> {
    Url::parse(value).ok()?.host_str().map(str::to_owned)
}
fn env_default(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}
fn required_env(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("{name} is required").into())
}
fn json_array(value: Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}
fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(v) => v.clone(),
        other => other.to_string(),
    }
}
fn present(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(v)) => !v.is_empty(),
        Some(Value::Array(v)) => !v.is_empty(),
        Some(Value::Object(v)) => !v.is_empty(),
        Some(_) => true,
    }
}
fn copy_present(source: &Value, target: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = source.get(from).filter(|v| present(Some(v))) {
        target.insert(to.to_owned(), value.clone());
    }
}
fn db_error(error: sqlx::Error) -> ApiError {
    ApiError::internal(format!("database operation failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_match_the_proxy_shape() {
        let rules = vec![
            json!({"host":"api.example.com","cidr":null,"http_methods":["POST"],"paths":["/v1/*"],"position":0}),
        ];
        assert_eq!(
            proxy_rules(&rules),
            json!([{"host":"api.example.com","methods":["POST"],"paths":["/v1/*"]}])
        );
    }

    #[test]
    fn higher_priority_conflict_suppresses_lower_priority() {
        let make = |kind: &str, id, priority| Credential {
            kind: kind.to_owned(),
            id,
            priority,
            data: if kind == "static" {
                json!({"inject_config":{"header":"Authorization"}})
            } else {
                json!({})
            },
            sources: vec![],
            rules: vec![json!({"host":"api.example.com"})],
        };
        let mut credentials = vec![make("gcp_auth", 1, 0), make("static", 2, 100)];
        suppress_conflicts(&mut credentials);
        assert_eq!(credentials.len(), 1);
        assert_eq!(credentials[0].kind, "static");
    }

    #[test]
    fn windowed_tokens_are_stable_within_a_window() {
        let start = window_start("prn_example", 1_700_000_001, 900);
        assert_eq!(start, window_start("prn_example", start + 899, 900));
    }

    #[test]
    fn opaque_ids_match_rails() {
        assert_eq!(oid("prn", 1), "prn_5CO4fITZ");
        assert_eq!(oid("prn", 3), "prn_xUC2fVYG");
        assert_eq!(oid("prn", 123), "prn_yRoWctYw");
    }
}
