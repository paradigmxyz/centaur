//! Typed request parsing for the Console API.
//!
//! Console's Rails controllers read `params[:data]` as a hash and rely on
//! ActiveRecord's type casting for everything else. The Rust port instead
//! deserializes each endpoint into a narrow serde struct, so a field is either
//! the declared JSON type or a validation error; there is no implicit
//! `"true"`/`1`/`""` to boolean or integer conversion.
//!
//! Four extractors cover the routes in this tree:
//!
//! | Extractor | Wire shape |
//! | --- | --- |
//! | [`ConsoleData`] | JSON `{"data":{...}}` (Console's document writes) |
//! | [`ConsoleBody`] | a plain JSON body with no `data` wrapper |
//! | [`ConsoleQuery`] | typed query parameters |
//! | [`ConsoleForm`] | `application/x-www-form-urlencoded` forms |
//!
//! Unknown fields are always ignored (the `return_to[]` form key is exactly
//! that: absent, not a sentinel). A repeated scalar key is a 400 instead of
//! Rack's last-wins, while bracket-shaped keys stay Rack's array/hash forms. A
//! missing `data` is the port's 400 `missing required "data" object`; a field
//! with the wrong type is a 422 `validation failed` envelope with the field and
//! serde's message under `details`.
//!
//! ```ignore
//! use serde::Deserialize;
//! use crate::console::request::ConsoleData;
//!
//! #[derive(Deserialize)]
//! struct CreateRole {
//!     name: String,
//!     #[serde(default)]
//!     admin: Option<bool>,
//! }
//!
//! async fn create(ConsoleData(data): ConsoleData<CreateRole>) -> Response {
//!     // `data.name` is a String and `data.admin` is a bool/none; unknown
//!     // fields in `{"data":{...}}` are ignored.
//!     // ...
//! }
//! ```

use axum::{
    body::Bytes,
    extract::{FromRequest, FromRequestParts, RawForm, Request},
    http::{StatusCode, request::Parts},
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

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
/// field and is ignored, matching the decision to drop the Rack `return_to[]`
/// sentinel imitation.
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

        let data = value.get("data");
        match data {
            // Rails' `params.require(:data)` treats nil, false, and empty
            // scalars/containers as "missing".
            None | Some(Value::Null | Value::Bool(false)) => return Err(missing_data()),
            Some(Value::Object(map)) if map.is_empty() => return Err(missing_data()),
            Some(Value::String(value)) if value.is_empty() => return Err(missing_data()),
            Some(Value::Array(values)) if values.is_empty() => return Err(missing_data()),
            Some(Value::Object(_)) => {}
            Some(_) => {
                return Err(ConsoleError::validation_failed(
                    json!({ "data": ["must be an object"] }),
                ));
            }
        }

        let data = data.expect("matched an object above");
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
        let raw = collapse_duplicates(parts.uri.query().unwrap_or_default().as_bytes(), "query")?;
        let value = deserialize_urlencoded(&raw)?;
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
        let raw = collapse_duplicates(&bytes, "form")?;
        let value = deserialize_urlencoded(&raw)?;
        Ok(Self(value))
    }
}

fn missing_data() -> ConsoleError {
    ConsoleError::bad_request(MISSING_DATA_MESSAGE)
}

/// The plain replacement for Rails' `ParameterMissing` text. The status stays a
/// 400 in every namespace; only the framework-specific wording is dropped.
pub(crate) const MISSING_DATA_MESSAGE: &str = "missing required \"data\" object";

fn deserialize_urlencoded<T: DeserializeOwned>(raw: &str) -> Result<T, ConsoleError> {
    let deserializer =
        serde_urlencoded::Deserializer::new(url::form_urlencoded::parse(raw.as_bytes()));
    serde_path_to_error::deserialize(deserializer).map_err(validation_error)
}

/// True when the raw query/form string contains a Rack-style nested parameter
/// for `name`, such as `scopes[]` or `scopes[0]`.
///
/// A typed struct ignores those keys, which is right for an unknown field but
/// wrong for one whose absence selects a broader default. A handler that reads
/// such a field calls this before applying the default, so a nested key is a
/// client error rather than a silent widening.
pub(crate) fn has_nested_param(raw: Option<&str>, name: &str) -> bool {
    let Some(raw) = raw else {
        return false;
    };
    url::form_urlencoded::parse(raw.as_bytes()).any(|(key, _)| {
        key.strip_prefix(name)
            .is_some_and(|rest| rest.starts_with('['))
    })
}

/// Collapses repeated scalar keys to their last value (Rack keeps the last
/// simple parameter) and re-encodes the pairs for serde. A repeated scalar key
/// is refused instead: the two values disagree and no handler consumes both.
/// Bracket-shaped keys such as `return_to[]` or `labels[team]` stay separate,
/// so a flat struct ignores them and Rack's array/hash forms keep working.
fn collapse_duplicates(raw: &[u8], source: &str) -> Result<String, ConsoleError> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (key, value) in url::form_urlencoded::parse(raw) {
        let scalar = !key.contains('[');
        if scalar && !seen.insert(key.clone().into_owned()) {
            return Err(ConsoleError::bad_request(format!(
                "{source} parameter must not be repeated: {key}"
            )));
        }
        if let Some(existing) = pairs
            .iter_mut()
            .find(|(existing, _)| existing.as_str() == key.as_ref())
        {
            existing.1 = value.into_owned();
        } else {
            pairs.push((key.into_owned(), value.into_owned()));
        }
    }
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.extend_pairs(&pairs);
    Ok(serializer.finish())
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
            r#"{"data":{}}"#,
            r#"{"data":[]}"#,
        ] {
            let error = data_request(body).await.unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(error.message, MISSING_DATA_MESSAGE, "{body}");
        }
        let error = data_request(r#"{"data":"value"}"#).await.unwrap_err();
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            error.details,
            Some(json!({ "data": ["must be an object"] }))
        );
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

        // No ActiveModel casting: a JSON string is not a boolean.
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

    #[test]
    fn nested_parameter_detection_is_name_exact() {
        assert!(has_nested_param(Some("scopes[]=read"), "scopes"));
        assert!(has_nested_param(Some("scopes%5B0%5D=read"), "scopes"));
        assert!(has_nested_param(
            Some("q=x&scopes%5Bname%5D=read"),
            "scopes"
        ));
        assert!(!has_nested_param(Some("scopes=read"), "scopes"));
        assert!(!has_nested_param(Some("scopes2[]=read"), "scopes"));
        assert!(!has_nested_param(Some("my_scopes[]=read"), "scopes"));
        assert!(!has_nested_param(None, "scopes"));
    }

    #[tokio::test]
    async fn console_query_is_typed_and_nested_keys_are_unknown() {
        assert_eq!(
            query_request("/login?return_to=%2Flaunch").await.unwrap(),
            ReturnTo {
                return_to: Some("/launch".to_owned()),
            }
        );
        assert_eq!(
            query_request("/login?return_to[]=%2Flaunch").await.unwrap(),
            ReturnTo { return_to: None }
        );
        // Rack keeps the last duplicate scalar; the extractor refuses it.
        let error = query_request("/login?return_to=%2Fconsole&return_to=%2Flaunch")
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.message,
            "query parameter must not be repeated: return_to"
        );
        // Rack's array form is not a scalar; the flat struct ignores it.
        assert_eq!(
            query_request("/login?ids[]=a&ids[]=b").await.unwrap(),
            ReturnTo { return_to: None }
        );
    }

    #[tokio::test]
    async fn console_form_ignores_nested_keys_and_reports_bad_types() {
        assert_eq!(
            form_request("return_to=%2Flaunch").await.unwrap(),
            ReturnTo {
                return_to: Some("/launch".to_owned()),
            }
        );
        assert_eq!(
            form_request("return_to[]=%2Flaunch").await.unwrap(),
            ReturnTo { return_to: None }
        );
        let error = form_request("return_to=%2Fa&return_to=%2Fb")
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.message,
            "form parameter must not be repeated: return_to"
        );

        #[derive(Debug, Deserialize)]
        struct Flags {
            #[serde(default)]
            enabled: Option<bool>,
        }
        let request = Request::builder()
            .method("POST")
            .uri("/login")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from("enabled=1"))
            .unwrap();
        let error = ConsoleForm::<Flags>::from_request(request, &())
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::UNPROCESSABLE_ENTITY);
        let details = error.details.unwrap();
        assert_eq!(details["enabled"].as_array().unwrap().len(), 1, "{details}");
        assert!(
            details["enabled"][0].as_str().unwrap().contains("true"),
            "{details}"
        );
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
