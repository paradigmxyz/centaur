//! Shared handler helpers for the admin area: the handler error type, the
//! writable Console pool, the JSON response helpers, and the serde helper that
//! keeps a PATCH request's "field absent" distinct from "field explicit null".
//!
//! Timestamps, 404s, and validation details come from `centaur_console_db` and
//! `console::http`; request bodies are typed per endpoint with `console::request`
//! and validated with `centaur_console_db::validation`.

use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::PgPool;

use crate::{ApiError, AppState, console::http::ConsoleError};
use centaur_console_db::FieldErrors;

/// A handler failure: either a Console error or an api-rs error raised by the
/// shared session layer, such as an unavailable database.
#[derive(Debug)]
pub(crate) enum AdminError {
    Console(ConsoleError),
    Api(ApiError),
}

impl From<ConsoleError> for AdminError {
    fn from(error: ConsoleError) -> Self {
        Self::Console(error)
    }
}

impl From<ApiError> for AdminError {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

/// The data layer's validation failures render Console's 422 envelope.
impl From<FieldErrors> for AdminError {
    fn from(error: FieldErrors) -> Self {
        Self::Console(error.into())
    }
}

impl From<sqlx::Error> for AdminError {
    fn from(error: sqlx::Error) -> Self {
        Self::Console(error.into())
    }
}

impl From<centaur_console_db::Error> for AdminError {
    fn from(error: centaur_console_db::Error) -> Self {
        Self::Console(error.into())
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        match self {
            Self::Console(error) => error.into_response(),
            Self::Api(error) => error.into_response(),
        }
    }
}

/// The writable Console pool. Reads use it too: the admin screens act on rows
/// the same request may have just written.
pub(crate) fn pool(state: &AppState) -> Result<PgPool, AdminError> {
    Ok(state.console_session_db()?)
}

/// A typed body with no `data` envelope: Console's settings payloads and the
/// list bodies that carry their own `meta`.
pub(crate) fn json_ok(body: impl Serialize) -> Response {
    Json(body).into_response()
}

pub(crate) fn json_created(body: impl Serialize) -> Response {
    (StatusCode::CREATED, Json(body)).into_response()
}

/// Console's empty success body.
pub(crate) use crate::console::http::no_content;

/// A 422 `details` entry for one attribute, for a check that needs the database
/// or the stored row rather than the request alone.
pub(crate) fn field_error(attribute: &str, message: impl Into<String>) -> AdminError {
    let mut errors = FieldErrors::new();
    errors.add(attribute, message);
    errors.into()
}

/// A 422 on the record as a whole (ActiveModel's `base`).
pub(crate) fn base_error(message: impl Into<String>) -> AdminError {
    field_error(FieldErrors::BASE, message)
}

/// Console marks responses that carry a freshly issued secret `no-store`.
pub(crate) fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// A PATCH field that separates "absent" from "explicit null".
///
/// `#[serde(default)]` yields `None` when the key is absent and this
/// deserializer is skipped; a present `null` becomes `Some(None)` and a
/// present value `Some(Some(value))`. A wrong type is still a validation error.
pub(crate) fn nullable_field<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

// Rails' `String#blank?` for a required string attribute lives in the garde
// rules (`centaur_console_db::validation::rules`), which report it as the
// model's `can't be blank` message.

#[cfg(test)]
mod tests {
    use super::*;
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
}
