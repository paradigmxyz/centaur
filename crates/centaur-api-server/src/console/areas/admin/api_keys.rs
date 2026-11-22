//! Bearer `/api/v1/api_keys`: the caller's own API keys.
//!
//! The key used for the request cannot revoke itself.
//!
//! This resource is the Console abstractions' reference slice: the response is a
//! typed [`ApiKeyPayload`], the list goes through `http::list`, validation
//! returns [`FieldErrors`], and the soft delete is the model descriptor's.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    response::Response,
    routing::get,
};
use centaur_console_db::areas::admin::{
    API_KEY_LIST_FROM, API_KEY_LIST_ORDER, API_KEY_LIST_SELECT, ApiKeyRow, api_key, create_api_key,
    soft_delete_api_key,
};
use centaur_console_db::model::{ApiKey as ApiKeyModel, ConsoleModel};
use centaur_console_db::validation::{ValidateFields, rules};
use serde::{Deserialize, Serialize};
use time::PrimitiveDateTime;

use super::common::{self, json_ok};
use crate::{
    AppState,
    console::{
        guard::{BearerApiKey, BearerUser, GuardedRoutes},
        http::{
            ConsoleError, ListParams, ListSource, bind, created_data, json_data, list, no_content,
        },
        request::ConsoleData,
    },
};

const TOKEN_PREFIX: &str = "iak_";

const KEY_LIST: ListSource = ListSource::new(
    API_KEY_LIST_SELECT,
    API_KEY_LIST_FROM,
    API_KEY_LIST_ORDER,
    1,
);

/// A bearer API key is created from its name alone.
#[derive(Deserialize, garde::Validate)]
struct CreateApiKey {
    #[serde(default)]
    #[garde(custom(rules::present_not_blank))]
    name: Option<String>,
}

/// The `data` object of every API-key response; `token` is present only in the
/// create response.
#[derive(Serialize)]
struct ApiKeyPayload {
    id: String,
    name: String,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    created_at: PrimitiveDateTime,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    updated_at: PrimitiveDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

impl From<&ApiKeyRow> for ApiKeyPayload {
    fn from(key: &ApiKeyRow) -> Self {
        Self {
            id: ApiKeyModel::oid(key.id),
            name: key.name.clone(),
            created_at: key.created_at,
            updated_at: key.updated_at,
            token: None,
        }
    }
}

fn payload(key: &ApiKeyRow) -> ApiKeyPayload {
    ApiKeyPayload::from(key)
}

pub(super) fn bearer(routes: GuardedRoutes) -> GuardedRoutes {
    routes
        .operator("/api_keys", get(index).post(create))
        .operator("/api_keys/{id}", get(show).delete(destroy))
}

async fn index(
    State(state): State<AppState>,
    user: BearerUser,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    let params = ListParams::from_query(&query)?;
    let pool = common::pool(&state)?;
    let body = list::<ApiKeyRow, ApiKeyPayload, _, _>(
        &pool,
        &params,
        KEY_LIST,
        |arguments| bind(arguments, user.0.id),
        payload,
    )
    .await?;
    Ok(json_ok(body))
}

async fn show(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: BearerUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let key = load(&pool, user.0.id, &id).await?;
    Ok(json_data(payload(&key)))
}

async fn create(
    State(state): State<AppState>,
    user: BearerUser,
    ConsoleData(data): ConsoleData<CreateApiKey>,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    data.validate_fields()?;
    let name = data.name.unwrap_or_default();
    let token = common::issue_token(TOKEN_PREFIX);
    let key = create_api_key(&pool, user.0.id, &name, &common::token_digest(&token)).await?;
    // The freshly minted token is the only field the listing never renders.
    let mut payload = payload(&key);
    payload.token = Some(token);
    Ok(created_data(payload))
}

async fn destroy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    api_key: BearerApiKey,
    user: BearerUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let key = load(&pool, user.0.id, &id).await?;
    // Console compares against the key this request authenticated with; the
    // bearer guard attaches that key's id.
    if api_key.0 == key.id {
        return Err(ConsoleError::unprocessable(
            "cannot revoke the API key used for this request",
        ));
    }
    soft_delete_api_key(&pool, key.id).await?;
    Ok(no_content())
}

async fn load(pool: &sqlx::PgPool, user_id: i64, oid: &str) -> Result<ApiKeyRow, ConsoleError> {
    Ok(api_key(pool, user_id, common::oid::<ApiKeyModel>(oid)?)
        .await?
        .ok_or_else(ApiKeyModel::not_found)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_tokens_use_the_api_key_prefix() {
        assert_eq!(TOKEN_PREFIX, "iak_");
        assert!(common::issue_token(TOKEN_PREFIX).starts_with(TOKEN_PREFIX));
    }

    #[test]
    fn a_key_name_is_required() {
        let rejected = |body: serde_json::Value| common::detail_keys::<CreateApiKey>(body);
        assert_eq!(
            rejected(serde_json::json!({})),
            Some(vec!["name".to_owned()])
        );
        assert_eq!(
            rejected(serde_json::json!({ "name": "  " })),
            Some(vec!["name".to_owned()])
        );
        assert_eq!(rejected(serde_json::json!({ "name": "runner" })), None);
    }
}
