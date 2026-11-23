//! Typed request parsing for the Console API.
//!
//! | Extractor | Wire shape |
//! | --- | --- |
//! | [`ConsoleData`] | JSON `{"data":{...}}` (Console's document writes) |
//! | [`ConsoleBody`] | a plain JSON body with no `data` wrapper |
//! | [`ConsoleQuery`] | typed query parameters |
//! | [`ConsoleForm`] | `application/x-www-form-urlencoded` forms |
//!
//! A missing `data`, or a `data` value that is not a non-empty object, is the
//! 400 `missing required "data" object`; a field with the wrong type is a 422
//! `validation failed` envelope with the field and serde's message under
//! `details`.

use axum::{
    body::Bytes,
    extract::{FromRequest, FromRequestParts, RawForm, Request},
    http::{StatusCode, request::Parts},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use crate::console::http::ConsoleError;

/// A JSON body of the form `{"data": {...}}` deserialized into `T`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConsoleData<T>(pub(crate) T);

/// A plain JSON body (no `data` wrapper) deserialized into `T`.
///
/// An empty body is treated as `{}` so all-optional structs remain usable for
/// endpoints that accept a bare post.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConsoleBody<T>(pub(crate) T);

/// Typed query parameters. `labels[key]` filters are not representable in a
/// struct; paginated label searches keep `Pagination`/`ListParams` from
/// `console/http.rs`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConsoleQuery<T>(pub(crate) T);

/// A URL-encoded form body deserialized into `T`.
///
/// Form structs are flat: a nested key such as `return_to[]` is an unknown
/// field and is ignored; there is no `return_to[]` sentinel.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConsoleForm<T>(pub(crate) T);

impl<S, T> FromRequest<S> for ConsoleData<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ConsoleError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|error| ConsoleError::bad_request(format!("could not read body: {error}")))?;
        let value = if bytes.iter().all(u8::is_ascii_whitespace) {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|error| {
                ConsoleError::bad_request(format!("malformed JSON request body: {error}"))
            })?
        };

        let data = match value.get("data") {
            Some(Value::Object(map)) if !map.is_empty() => map,
            _ => return Err(missing_data()),
        };
        let value = serde_path_to_error::deserialize::<_, T>(data).map_err(validation_error)?;
        Ok(Self(value))
    }
}

impl<S, T> FromRequest<S> for ConsoleBody<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ConsoleError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|error| ConsoleError::bad_request(format!("could not read body: {error}")))?;
        let value = if bytes.iter().all(u8::is_ascii_whitespace) {
            Value::Object(Map::new())
        } else {
            serde_json::from_slice(&bytes).map_err(|error| {
                ConsoleError::bad_request(format!("malformed JSON request body: {error}"))
            })?
        };
        let value = serde_path_to_error::deserialize::<_, T>(&value).map_err(validation_error)?;
        Ok(Self(value))
    }
}

impl<S, T> FromRequestParts<S> for ConsoleQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ConsoleError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let value = parse_urlencoded(parts.uri.query().unwrap_or_default())?;
        Ok(Self(value))
    }
}

impl<S, T> FromRequest<S> for ConsoleForm<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ConsoleError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let RawForm(bytes) = RawForm::from_request(request, state).await.map_err(|_| {
            ConsoleError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "expected an application/x-www-form-urlencoded body",
            )
        })?;
        let raw = std::str::from_utf8(&bytes)
            .map_err(|error| ConsoleError::bad_request(format!("could not read body: {error}")))?;
        let value = parse_urlencoded(raw)?;
        Ok(Self(value))
    }
}

fn missing_data() -> ConsoleError {
    ConsoleError::bad_request(MISSING_DATA_MESSAGE)
}

/// The plain 400 message for a missing `data` object. The status stays a 400 in
/// every namespace.
pub(crate) const MISSING_DATA_MESSAGE: &str = "missing required \"data\" object";

fn parse_urlencoded<T: DeserializeOwned>(raw: &str) -> Result<T, ConsoleError> {
    serde_urlencoded::from_str(raw).map_err(|error| ConsoleError::bad_request(error.to_string()))
}

/// Maps a serde path error to Console's `validation failed` envelope. The
/// details key is the field path, so an area handler can return it as-is.
fn validation_error<E: std::fmt::Display>(error: serde_path_to_error::Error<E>) -> ConsoleError {
    let path = error.path().to_string();
    let message = error.inner().to_string();
    let (field, message) = field_and_message(&path, &message);
    let mut errors = centaur_console_db::FieldErrors::new();
    errors.add(&field, message);
    errors.into()
}

fn field_and_message(path: &str, message: &str) -> (String, String) {
    let path = match path.trim() {
        "" | "." => None,
        path => Some(path.to_owned()),
    };
    if let Some(rest) = message.strip_prefix("missing field `")
        && let Some((name, _)) = rest.split_once('`')
    {
        let field = match &path {
            Some(path) => format!("{path}.{name}"),
            None => name.to_owned(),
        };
        return (field, "is required".to_owned());
    }
    if let Some(rest) = message.strip_prefix("duplicate field `")
        && let Some((name, _)) = rest.split_once('`')
    {
        return (name.to_owned(), "must not be repeated".to_owned());
    }
    (
        path.unwrap_or_else(|| "data".to_owned()),
        message.to_owned(),
    )
}

/// A PATCH field that separates "absent" from "explicit null".
///
/// `#[serde(default)]` yields `None` when the key is absent and this
/// deserializer is skipped; a present `null` becomes `Some(None)` and a
/// present value `Some(Some(value))`. A wrong type is still a validation error.
pub(crate) fn nullable_field<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        extract::{FromRequest, FromRequestParts},
        http::{Request, StatusCode, header},
    };
    use serde::Deserialize;
    use serde_json::{Value, json};

    use super::*;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Payload {
        name: String,
        #[serde(default)]
        enabled: Option<bool>,
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct ReturnTo {
        #[serde(default)]
        return_to: Option<String>,
    }

    async fn data_request(body: &str) -> Result<Payload, ConsoleError> {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/console/things")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap();
        ConsoleData::<Payload>::from_request(request, &())
            .await
            .map(|ConsoleData(payload)| payload)
    }

    async fn query_request(uri: &str) -> Result<ReturnTo, ConsoleError> {
        let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let (mut parts, _) = request.into_parts();
        ConsoleQuery::<ReturnTo>::from_request_parts(&mut parts, &())
            .await
            .map(|ConsoleQuery(query)| query)
    }

    async fn form_request(body: &str) -> Result<ReturnTo, ConsoleError> {
        let request = Request::builder()
            .method("POST")
            .uri("/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_owned()))
            .unwrap();
        ConsoleForm::<ReturnTo>::from_request(request, &())
            .await
            .map(|ConsoleForm(form)| form)
    }

    #[tokio::test]
    async fn console_data_requires_a_non_empty_data_object() {
        for body in [
            "",
            "{}",
            r#"{"data":null}"#,
            r#"{"data":false}"#,
            r#"{"data":true}"#,
            r#"{"data":1}"#,
            r#"{"data":"value"}"#,
            r#"{"data":[]}"#,
            r#"{"data":{}}"#,
        ] {
            let error = data_request(body).await.unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(error.message, MISSING_DATA_MESSAGE, "{body}");
            assert_eq!(error.details, None, "{body}");
        }
        let error = data_request("not json").await.unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.starts_with("malformed JSON request body:"));
    }

    #[tokio::test]
    async fn console_data_ignores_unknown_fields_and_keeps_strict_types() {
        assert_eq!(
            data_request(r#"{"data":{"name":"role","extra":{"nested":true}},"other":1}"#)
                .await
                .unwrap(),
            Payload {
                name: "role".to_owned(),
                enabled: None,
            }
        );
        assert_eq!(
            data_request(r#"{"data":{"name":"role","enabled":true}}"#)
                .await
                .unwrap()
                .enabled,
            Some(true)
        );

        // No casting: a JSON string is not a boolean.
        let error = data_request(r#"{"data":{"name":"role","enabled":"true"}}"#)
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.message, "validation failed");
        let details = error.details.unwrap();
        assert_eq!(details["enabled"].as_array().unwrap().len(), 1, "{details}");
        assert!(
            details["enabled"][0]
                .as_str()
                .unwrap()
                .contains("expected a boolean"),
            "{details}"
        );

        // A missing required field names the field instead of the whole body.
        let error = data_request(r#"{"data":{"enabled":true}}"#)
            .await
            .unwrap_err();
        let details = error.details.unwrap();
        assert_eq!(
            details.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["name"]
        );
    }

    #[tokio::test]
    async fn console_query_is_typed() {
        assert_eq!(
            query_request("/login?return_to=%2Fconsole").await.unwrap(),
            ReturnTo {
                return_to: Some("/console".to_owned()),
            }
        );
        // A nested key is just an unknown field for a flat struct.
        assert_eq!(
            query_request("/login?return_to[]=%2Fconsole")
                .await
                .unwrap(),
            ReturnTo { return_to: None }
        );
        let error = query_request("/login?return_to=%2Fa&return_to=%2Fb")
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn console_form_is_typed_and_reports_bad_values() {
        assert_eq!(
            form_request("return_to=%2Fconsole").await.unwrap(),
            ReturnTo {
                return_to: Some("/console".to_owned()),
            }
        );

        let request = Request::builder()
            .method("POST")
            .uri("/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("enabled=1"))
            .unwrap();
        let error =
            ConsoleForm::<std::collections::HashMap<String, bool>>::from_request(request, &())
                .await
                .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(!error.message.is_empty());
    }

    #[tokio::test]
    async fn console_body_accepts_an_empty_body_as_an_empty_object() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Optional {
            #[serde(default)]
            principal: Option<String>,
        }
        for body in ["", "{}"] {
            let request = Request::builder()
                .method("POST")
                .uri("/api/v1/console/api_clients/x/principals")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap();
            assert_eq!(
                ConsoleBody::<Optional>::from_request(request, &())
                    .await
                    .unwrap()
                    .0,
                Optional { principal: None },
                "{body}"
            );
        }
    }

    #[test]
    fn validation_envelope_keeps_console_shape() {
        let Value::Object(details) = json!({ "enabled": ["is required"] }) else {
            unreachable!()
        };
        let error = ConsoleError::validation_failed(Value::Object(details));
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(error.message, "validation failed");
        assert_eq!(error.details, Some(json!({ "enabled": ["is required"] })));
    }
}
