mod active_record_encryption;
mod config;
mod conflicts;
mod database;
mod identifiers;
mod models;
mod tokens;

use std::{env, net::SocketAddr, sync::Arc};

use active_record_encryption::{ActiveRecordEncryption, Error as EncryptionError};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use config::{build_config, config_hash};
use database::load_proxy;
use identifiers::oid;
use models::{AppState, Config};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use url::Url;

#[derive(Deserialize, Default)]
struct SyncRequest {
    config_hash: Option<String>,
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
                .and_then(|value| value.parse().ok())
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
            .filter(|value| !value.trim().is_empty()),
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
    if request
        .config_hash
        .as_deref()
        .filter(|value| !value.is_empty())
        == Some(config_hash.as_str())
    {
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
        .filter(|value| !value.is_empty())
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
        .map(|host| host.trim().trim_end_matches('.').to_lowercase())
        .filter(|host| !host.is_empty())
        .fold(Vec::new(), |mut output, host| {
            if !output.contains(&host) {
                output.push(host);
            }
            output
        })
}

fn host_from_url(value: &str) -> Option<String> {
    Url::parse(value).ok()?.host_str().map(str::to_owned)
}

fn required_env(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("{name} is required").into())
}
