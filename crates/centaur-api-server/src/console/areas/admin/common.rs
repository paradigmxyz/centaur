//! Shared handler helpers for the admin area: the writable Console pool and
//! the JSON response helpers. `console::request::nullable_field` keeps a PATCH
//! request's "field absent" distinct from "field explicit null".
//!
//! Timestamps, 404s, and validation details come from `centaur_console_db` and
//! `console::http`; request bodies are typed per endpoint with `console::request`
//! and validated with `centaur_console_db::validation`.

use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::{AppState, console::http::ConsoleError};
use centaur_console_db::FieldErrors;
use centaur_console_db::model::ConsoleModel;

/// The writable Console pool. Reads use it too: the admin screens act on rows
/// the same request may have just written.
pub(crate) fn pool(state: &AppState) -> Result<PgPool, ConsoleError> {
    // The only failure here is an unconfigured Console database.
    state.console_session_db().map_err(|_| {
        ConsoleError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "the console database is not configured",
        )
    })
}

/// A typed body with no `data` envelope: the two list bodies that carry their
/// own `meta`. Everything else uses [`crate::console::http::json_data`].
pub(crate) fn json_ok(body: impl Serialize) -> Response {
    Json(body).into_response()
}

/// A 422 `details` entry for one attribute, for a check that needs the database
/// or the stored row rather than the request alone.
pub(crate) fn field_error(attribute: &str, message: impl Into<String>) -> ConsoleError {
    let mut errors = FieldErrors::new();
    errors.add(attribute, message);
    errors.into()
}

/// Console marks responses that carry a freshly issued secret `no-store`.
pub(crate) fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// A freshly issued bearer credential: `prefix` followed by 32 random bytes, as
/// Console's `SecureRandom.hex(32)` renders them.
pub(crate) fn issue_token(prefix: &str) -> String {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).expect("operating system randomness");
    format!("{prefix}{}", hex::encode(bytes))
}

/// The stored digest of an issued credential: its SHA-256, hex encoded.
pub(crate) fn token_digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// The row id an opaque path segment names, or the model's plain 404.
pub(crate) fn oid<M: ConsoleModel>(oid: &str) -> Result<i64, ConsoleError> {
    M::decode_oid(oid)
        .ok_or_else(M::not_found)
        .map_err(ConsoleError::from)
}

pub(crate) use crate::console::request::nullable_field;

// The blank check for a required string attribute lives in the garde rules
// (`centaur_console_db::validation::rules`), which report it as the
// `can't be blank` message.

/// The `details` keys a request body is rejected under, or `None` when it
/// passes, sorted so a test does not pin report order.
#[cfg(test)]
pub(crate) fn detail_keys<T>(body: serde_json::Value) -> Option<Vec<String>>
where
    T: serde::de::DeserializeOwned + centaur_console_db::validation::ValidateFields,
    <T as garde::Validate>::Context: Default,
{
    let request: T = serde_json::from_value(body).expect("a typed request");
    request.validate_fields().err().map(|errors| {
        let mut keys: Vec<String> = errors
            .entries()
            .map(|(attribute, _)| attribute.to_owned())
            .collect();
        keys.sort();
        keys
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Deserialize, PartialEq, Debug)]
    struct Patchable {
        #[serde(default, deserialize_with = "nullable_field")]
        name: Option<Option<String>>,
    }

    #[test]
    fn nullable_field_keeps_absent_and_null_apart() {
        assert_eq!(
            serde_json::from_value::<Patchable>(json!({})).unwrap(),
            Patchable { name: None }
        );
        assert_eq!(
            serde_json::from_value::<Patchable>(json!({ "name": null })).unwrap(),
            Patchable { name: Some(None) }
        );
        assert_eq!(
            serde_json::from_value::<Patchable>(json!({ "name": "x" })).unwrap(),
            Patchable {
                name: Some(Some("x".to_owned())),
            }
        );
        assert!(serde_json::from_value::<Patchable>(json!({ "name": 5 })).is_err());
    }

    #[test]
    fn issued_tokens_are_a_prefix_and_32_random_bytes() {
        let token = issue_token("test_");
        assert!(token.starts_with("test_"));
        assert_eq!(token.len(), 5 + 64);
        assert!(
            token[5..]
                .chars()
                .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        );
        assert_ne!(issue_token("test_"), issue_token("test_"));
        assert_eq!(token_digest(&token).len(), 64);
    }
}
