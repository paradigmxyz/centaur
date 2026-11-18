//! The console's Slack channel picker (`GET /api/v1/console/slack_channels`,
//! admin): search the catalog, or look up channels by id.

use axum::{
    extract::{RawQuery, State},
    response::IntoResponse,
};
use centaur_console_db::decode_oid;
use http::{HeaderValue, header};
use serde::Serialize;
use serde_json::json;

use super::slack::{self, Catalog};
use crate::{ApiError, ApiResult, AppState, auth::ConsoleCaller};

const MAX_RESULTS: i64 = 20;
const MAX_IDS: usize = 200;

#[derive(Serialize, sqlx::FromRow)]
struct Channel {
    channel_id: String,
    name: String,
    private: bool,
}

/// One channel as the pickers show it.
#[derive(Serialize)]
struct ChannelOption {
    channel_id: String,
    name: String,
    private: bool,
    label: String,
    description: String,
}

impl From<Channel> for ChannelOption {
    fn from(channel: Channel) -> Self {
        Self {
            label: format!("#{}", channel.name),
            description: format!(
                "{} · {}",
                channel.channel_id,
                if channel.private { "Private" } else { "Public" }
            ),
            channel_id: channel.channel_id,
            name: channel.name,
            private: channel.private,
        }
    }
}

struct Params {
    ids: Vec<String>,
    q: Option<String>,
    exclude_owner: Option<String>,
}

/// `ids[]=A&ids[]=B` (or repeated `ids=`), `q`, and `exclude_owner`.
fn parse(raw: Option<&str>) -> Params {
    let mut params = Params {
        ids: Vec::new(),
        q: None,
        exclude_owner: None,
    };
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match key.as_ref() {
            "ids[]" | "ids" => {
                let id = value.trim().to_uppercase();
                if !id.is_empty() {
                    params.ids.push(id);
                }
            }
            "q" => params.q = Some(value.into_owned()),
            "exclude_owner" if !value.trim().is_empty() => {
                params.exclude_owner = Some(value.trim().to_owned());
            }
            _ => {}
        }
    }
    params
}

/// The permission column and id a `principal:<oid>` / `role:<oid>` names.
async fn owner(state: &AppState, value: &str) -> ApiResult<(&'static str, i64)> {
    let (kind, oid) = value.split_once(':').unwrap_or((value, ""));
    let (column, prefix, table) = match kind {
        "principal" => ("principal_id", "prn", "principals"),
        "role" => ("role_id", "role", "roles"),
        _ => {
            return Err(ApiError::BadRequest(
                "exclude_owner must be principal:<oid> or role:<oid>".into(),
            ));
        }
    };
    let id = decode_oid(prefix, oid).ok_or_else(ApiError::not_found)?;
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {table} WHERE id = $1)"
    ))
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    if exists {
        Ok((column, id))
    } else {
        Err(ApiError::not_found())
    }
}

async fn channels(state: &AppState, params: &Params) -> ApiResult<Vec<Channel>> {
    if !params.ids.is_empty() {
        let ids: Vec<&String> = params.ids.iter().take(MAX_IDS).collect();
        return Ok(sqlx::query_as(
            "SELECT channel_id, name, private FROM slack_bot_channels \
             WHERE channel_id = ANY($1) ORDER BY lower(name), channel_id ASC",
        )
        .bind(ids)
        .fetch_all(&state.db)
        .await?);
    }
    let excluded: Vec<String> = match &params.exclude_owner {
        None => Vec::new(),
        Some(value) => {
            let (column, id) = owner(state, value).await?;
            sqlx::query_scalar(&format!(
                "SELECT channel_id FROM slack_channel_permissions WHERE {column} = $1"
            ))
            .bind(id)
            .fetch_all(&state.db)
            .await?
        }
    };
    let needle = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| format!("%{}%", super::skills::escape_like(q)));
    Ok(sqlx::query_as(
        "SELECT channel_id, name, private FROM slack_bot_channels \
         WHERE active AND NOT archived AND NOT (channel_id = ANY($1)) \
         AND ($2::text IS NULL OR name ILIKE $2 OR channel_id ILIKE $2) \
         ORDER BY lower(name), channel_id ASC LIMIT $3",
    )
    .bind(&excluded)
    .bind(needle)
    .bind(MAX_RESULTS)
    .fetch_all(&state.db)
    .await?)
}

pub async fn index(
    State(state): State<AppState>,
    ConsoleCaller(_admin): ConsoleCaller,
    RawQuery(raw): RawQuery,
) -> ApiResult {
    let params = parse(raw.as_deref());
    let catalog = Catalog::from_state(&state);
    catalog.enqueue_if_empty().await;
    let found: Vec<ChannelOption> = channels(&state, &params)
        .await?
        .into_iter()
        .map(ChannelOption::from)
        .collect();
    let catalog_error = if !slack::configured(&state.config.env) {
        Some("SLACK_BOT_TOKEN is not configured.")
    } else if sqlx::query_scalar::<_, bool>("SELECT NOT EXISTS (SELECT 1 FROM slack_bot_channels)")
        .fetch_one(&state.db)
        .await?
    {
        Some("Slack channel catalog is loading. Enter a channel ID or reload shortly.")
    } else {
        None
    };
    let mut response =
        axum::Json(json!({ "data": found, "meta": { "catalog_error": catalog_error } }))
            .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn ids_arrive_as_repeated_bracket_keys() {
        let params = parse(Some("ids[]=c1&ids%5B%5D=%20g2&q=ops&exclude_owner=role:x"));
        assert_eq!(params.ids, ["C1", "G2"]);
        assert_eq!(params.q.as_deref(), Some("ops"));
        assert_eq!(params.exclude_owner.as_deref(), Some("role:x"));
        assert!(parse(None).ids.is_empty());
    }
}
