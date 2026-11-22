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
use sha2::{Digest, Sha256};
use time::PrimitiveDateTime;

use super::common::{self, AdminError, json_created, json_ok, no_content};
use crate::{
    AppState,
    console::{
        guard::{BearerApiKey, BearerUser, GuardedRoutes},
        http::{ConsoleError, ListParams, ListSource, bind, list},
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

#[derive(Serialize)]
struct ApiKeyPayloadEnvelope {
    data: ApiKeyPayload,
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
) -> Result<Response, AdminError> {
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
) -> Result<Response, AdminError> {
    let pool = common::pool(&state)?;
    let key = load(&pool, user.0.id, &id).await?;
    Ok(json_ok(ApiKeyPayloadEnvelope {
        data: payload(&key),
    }))
}

async fn create(
    State(state): State<AppState>,
    user: BearerUser,
    ConsoleData(data): ConsoleData<CreateApiKey>,
) -> Result<Response, AdminError> {
    let pool = common::pool(&state)?;
    data.validate_fields()?;
    let name = data.name.unwrap_or_default();
    let token = issue_token();
    let key = create_api_key(&pool, user.0.id, &name, &token_digest(&token)).await?;
    // The freshly minted token is the only field the listing never renders.
    let mut payload = payload(&key);
    payload.token = Some(token);
    Ok(json_created(ApiKeyPayloadEnvelope { data: payload }))
}

async fn destroy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    api_key: BearerApiKey,
    user: BearerUser,
) -> Result<Response, AdminError> {
    let pool = common::pool(&state)?;
    let key = load(&pool, user.0.id, &id).await?;
    // Console compares against the key this request authenticated with; the
    // bearer guard attaches that key's id.
    if api_key.0 == key.id {
        return Err(
            ConsoleError::unprocessable("cannot revoke the API key used for this request").into(),
        );
    }
    soft_delete_api_key(&pool, key.id).await?;
    Ok(no_content())
}

async fn load(pool: &sqlx::PgPool, user_id: i64, oid: &str) -> Result<ApiKeyRow, AdminError> {
    let id = ApiKeyModel::decode_oid(oid).ok_or_else(ApiKeyModel::not_found)?;
    Ok(api_key(pool, user_id, id)
        .await?
        .ok_or_else(ApiKeyModel::not_found)?)
}

fn issue_token() -> String {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).expect("operating system randomness");
    format!("{TOKEN_PREFIX}{}", hex::encode(bytes))
}

fn token_digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_tokens_have_the_console_shape() {
        let token = issue_token();
        assert!(token.starts_with("iak_"));
        assert_eq!(token.len(), 4 + 64);
        assert!(
            token[4..]
                .chars()
                .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        );
        assert_ne!(issue_token(), issue_token());
    }

    #[test]
    fn a_key_name_is_required() {
        let rejected = |body: serde_json::Value| {
            let create: CreateApiKey = serde_json::from_value(body).expect("a typed request");
            create.validate_fields().err().map(|errors| {
                errors
                    .entries()
                    .map(|(attribute, _)| attribute.to_owned())
                    .collect::<Vec<_>>()
            })
        };
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
