//! A small HTTP client for the OAuth area.
//!
//! It applies open/read/write timeouts and a body cap. The consent flow uses it
//! for the code exchange, provider identity lookups, dynamic client
//! registration, and the optional Mercator claim lookup. Requests never log
//! URLs with query strings, bodies, or tokens; only the resulting status reaches
//! a user-facing message.

use std::time::Duration;

use axum::http::header::CONTENT_TYPE;
use reqwest::Client;
use serde_json::Value;

static CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();

fn client() -> &'static Client {
    CLIENT.get_or_init(Client::new)
}

/// One response, with the body read under a fixed cap.
#[derive(Clone, Debug)]
pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    pub(crate) body: String,
    pub(crate) content_type: Option<String>,
}

impl HttpResponse {
    pub(crate) fn success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub(crate) fn json(&self) -> Option<Value> {
        serde_json::from_str(&self.body).ok()
    }
}

/// A form field, in request order.
pub(crate) type Form = Vec<(String, String)>;
/// A request header.
pub(crate) type Header = (String, String);

/// Sends a request with the timeout Console's call sites use. Errors are
/// transport or construction failures; HTTP status codes are returned as data.
pub(crate) async fn send(
    method: reqwest::Method,
    url: &str,
    headers: &[Header],
    form: Option<&Form>,
    json: Option<&Value>,
    timeout: Duration,
    max_body_bytes: usize,
) -> Result<HttpResponse, reqwest::Error> {
    let mut request = client().request(method, url).timeout(timeout);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(form) = form {
        let body = form
            .iter()
            .fold(
                url::form_urlencoded::Serializer::new(String::new()),
                |mut serializer, (key, value)| {
                    serializer.append_pair(key, value);
                    serializer
                },
            )
            .finish();
        request = request
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body);
    }
    if let Some(json) = json {
        request = request.json(json);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response.bytes().await?;
    let bytes = &bytes[..bytes.len().min(max_body_bytes)];
    Ok(HttpResponse {
        status,
        body: String::from_utf8_lossy(bytes).into_owned(),
        content_type,
    })
}

pub(crate) fn bearer(token: &str) -> Header {
    ("Authorization".to_owned(), format!("Bearer {token}"))
}

pub(crate) fn header(name: &str, value: &str) -> Header {
    (name.to_owned(), value.to_owned())
}
