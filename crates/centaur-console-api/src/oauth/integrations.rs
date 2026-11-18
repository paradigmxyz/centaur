//! `GET /api/v1/console/integrations`: what the Integrations page shows one
//! console user (any active account).

use axum::{extract::State, response::IntoResponse};
use centaur_console_db::oid;
use http::HeaderMap;
use serde_json::{Value, json};

use super::{apps, providers::Provider, request_base, start_url};
use crate::{ApiResult, AppState, auth::ConsoleCaller, extract::data};

#[derive(sqlx::FromRow)]
struct AppRow {
    id: i64,
    slug: String,
    provider: String,
    provider_config: Value,
    description: Option<String>,
    shared: bool,
    always_available: bool,
}

/// The newest credential of the user for an app.
#[derive(sqlx::FromRow)]
struct Connection {
    oauth_app_id: i64,
    dead: bool,
    scopes: Value,
}

fn usage(app: &AppRow) -> &'static str {
    if app.shared {
        "shared"
    } else if app.always_available {
        "always_available"
    } else {
        "direct_messages"
    }
}

/// Connected, unless the credential is dead or lacks a scope the provider
/// needs.
fn needs_reconnect(app: &AppRow, connection: &Connection) -> bool {
    if connection.dead {
        return true;
    }
    let granted: Vec<&str> = connection
        .scopes
        .as_array()
        .map(|scopes| scopes.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    Provider::for_app(&app.provider, &app.slug, &app.provider_config).is_some_and(|provider| {
        provider
            .required_scopes()
            .iter()
            .any(|required| !granted.contains(required))
    })
}

pub async fn index(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    headers: HeaderMap,
) -> ApiResult {
    // Admins also see shared apps.
    let apps: Vec<AppRow> = sqlx::query_as(
        "SELECT id, slug, provider, provider_config, description, shared, always_available \
         FROM oauth_apps WHERE enabled AND ($1 OR NOT shared) ORDER BY slug",
    )
    .bind(user.admin)
    .fetch_all(&state.db)
    .await?;
    let app_ids: Vec<i64> = apps.iter().map(|app| app.id).collect();
    // The user's own credentials (created by them, or minted for their
    // email), and any credential of a shared app; the newest update wins.
    let connections: Vec<Connection> = sqlx::query_as(
        "SELECT DISTINCT ON (oauth_app_id) oauth_app_id, dead, scopes FROM broker_credentials \
         WHERE oauth_app_id = ANY($1) AND (created_by_id = $2 OR provider_email = $3 \
           OR oauth_app_id IN (SELECT id FROM oauth_apps WHERE shared)) \
         ORDER BY oauth_app_id, updated_at DESC, id DESC",
    )
    .bind(&app_ids)
    .bind(user.id)
    .bind(&user.email)
    .fetch_all(&state.db)
    .await?;
    let base = request_base(&state, &headers);
    let apps_json = apps
        .iter()
        .map(|app| {
            let connection = connections
                .iter()
                .find(|connection| connection.oauth_app_id == app.id)
                .map(|connection| {
                    json!({"status": if needs_reconnect(app, connection) { "needs_reconnect" } else { "connected" }})
                });
            Ok(json!({
                "id": oid("oap", app.id),
                "slug": app.slug,
                "provider": app.provider,
                "description": app.description,
                "shared": app.shared,
                "always_available": app.always_available,
                "usage": usage(app),
                "start_url": start_url(&base, &app.slug)?,
                "connection": connection,
            }))
        })
        .collect::<Result<Vec<_>, crate::ApiError>>()?;
    let presets: Vec<Value> = if user.admin {
        apps::available_presets(&state)
            .await?
            .iter()
            .map(|provider| {
                json!({
                    "provider": provider.key_str(),
                    "display_name": provider.display_name(),
                    "description": provider.preset().map(|preset| preset.description),
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    let ssh_key: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ssh_key_secrets WHERE owner_id = $1)")
            .bind(user.id)
            .fetch_one(&state.db)
            .await?;
    Ok(data(json!({
        "apps": apps_json,
        "presets": presets,
        "ssh_key": {"configured": ssh_key},
    }))
    .into_response())
}
