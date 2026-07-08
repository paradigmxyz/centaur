use std::{collections::BTreeSet, sync::OnceLock, time::Duration};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    ApiError,
    api_jwt::{bearer_token, verify_console_jwt},
    routes::{AppState, non_empty_env, positive_env_u64},
};

const DEFAULT_SLACK_API_URL: &str = "https://slack.com/api";
const DEFAULT_MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(60);

fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
            .read_timeout(HTTP_READ_TIMEOUT)
            .build()
            .expect("reqwest client configuration is valid")
    })
}

pub(crate) fn slack_proxy_router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/slack/files/upload",
            post(upload_slack_file).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/slack/files/{file_id}/download",
            get(download_slack_file),
        )
}

#[derive(Debug, Deserialize)]
struct SlackFileUploadQuery {
    channel_id: String,
    filename: String,
    #[serde(default)]
    thread_ts: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    initial_comment: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    alt_txt: Option<String>,
    #[serde(default)]
    snippet_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackFileDownloadQuery {
    channel_id: String,
}

#[derive(Debug, Deserialize)]
struct SlackFileProxyClaims {
    slack: SlackProxyClaims,
}

#[derive(Debug, Deserialize)]
struct SlackProxyClaims {
    #[serde(default)]
    upload_channels: Vec<String>,
    #[serde(default)]
    download_channels: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SlackFileUploadResponse {
    ok: bool,
    file_id: String,
    channel_id: String,
    thread_ts: Option<String>,
    file: Value,
}

async fn upload_slack_file(
    headers: HeaderMap,
    Query(query): Query<SlackFileUploadQuery>,
    body: Body,
) -> Result<Json<SlackFileUploadResponse>, ApiError> {
    let claims = authorize_slack_file_proxy(&headers)?;
    ensure_upload_channel_allowed(&claims, &query.channel_id)?;
    validate_slack_channel_id(&query.channel_id)?;
    validate_filename(&query.filename)?;
    if let Some(thread_ts) = query.thread_ts.as_deref() {
        validate_slack_thread_ts(thread_ts)?;
    }
    if let Some(content_type) = query.content_type.as_deref() {
        validate_content_type(content_type)?;
    }
    let config = slack_proxy_config()?;
    let content_length = content_length(&headers)?;
    ensure_upload_size(content_length, config.max_upload_bytes)?;
    let client = http_client();
    let upload_ticket = get_upload_url(
        client,
        config,
        &query.filename,
        content_length,
        query.alt_txt.as_deref(),
        query.snippet_type.as_deref(),
    )
    .await?;
    upload_file_bytes(
        client,
        &upload_ticket.upload_url,
        body,
        content_length,
        query.content_type.as_deref(),
    )
    .await?;
    let file = complete_upload(
        client,
        config,
        &upload_ticket.file_id,
        &query.channel_id,
        query.thread_ts.as_deref(),
        query.title.as_deref().unwrap_or(&query.filename),
        query.initial_comment.as_deref(),
    )
    .await?;

    Ok(Json(SlackFileUploadResponse {
        ok: true,
        file_id: upload_ticket.file_id,
        channel_id: query.channel_id,
        thread_ts: query.thread_ts,
        file,
    }))
}

async fn download_slack_file(
    headers: HeaderMap,
    Path(file_id): Path<String>,
    Query(query): Query<SlackFileDownloadQuery>,
) -> Result<Response, ApiError> {
    let claims = authorize_slack_file_proxy(&headers)?;
    ensure_download_channel_allowed(&claims, &query.channel_id)?;
    validate_slack_channel_id(&query.channel_id)?;
    validate_slack_file_id(&file_id)?;

    let config = slack_proxy_config()?;
    let client = http_client();
    let file = slack_file_info(client, config, &file_id).await?;
    if !slack_file_in_channel(&file, &query.channel_id) {
        return Err(ApiError::Forbidden(
            "file is not shared in an allowed Slack channel".to_owned(),
        ));
    }
    let download_url = file
        .get("url_private_download")
        .or_else(|| file.get("url_private"))
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::BadRequest("Slack file has no download URL".to_owned()))?;

    let upstream = client
        .get(download_url)
        .bearer_auth(&config.bot_token)
        .send()
        .await
        .map_err(|error| ApiError::Internal(format!("Slack file download failed: {error}")))?;
    if !upstream.status().is_success() {
        return Err(ApiError::BadRequest(format!(
            "Slack file download failed with status {}",
            upstream.status().as_u16()
        )));
    }

    let file_mimetype = file.get("mimetype").and_then(Value::as_str);
    // Slack's file host serves login/error pages with a 200 status; without this
    // check they would stream through labeled as the file's real mimetype.
    let upstream_content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    if upstream_body_is_unexpected_html(upstream_content_type, file_mimetype) {
        return Err(ApiError::Internal(
            "Slack file download returned an HTML page instead of the file contents".to_owned(),
        ));
    }

    let upstream_content_length = upstream.headers().get(header::CONTENT_LENGTH).cloned();
    let mut response = Body::from_stream(upstream.bytes_stream()).into_response();
    let headers = response.headers_mut();
    if let Some(value) = file_mimetype.and_then(|value| value.parse().ok()) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Some(value) = upstream_content_length {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    let filename = file
        .get("name")
        .or_else(|| file.get("title"))
        .and_then(Value::as_str)
        .unwrap_or(&file_id);
    if let Ok(value) = content_disposition_filename(filename).parse::<HeaderValue>() {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

fn upstream_body_is_unexpected_html(
    upstream_content_type: Option<&str>,
    file_mimetype: Option<&str>,
) -> bool {
    let upstream_is_html = upstream_content_type.is_some_and(|value| {
        value
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("text/html")
    });
    let file_is_html = file_mimetype.is_some_and(|value| value.eq_ignore_ascii_case("text/html"));
    upstream_is_html && !file_is_html
}

// No Debug derive: bot_token must not end up in logs via {:?} formatting.
struct SlackFileProxyConfig {
    api_url: String,
    bot_token: String,
    max_upload_bytes: u64,
}

fn slack_proxy_config() -> Result<&'static SlackFileProxyConfig, ApiError> {
    static CELL: OnceLock<SlackFileProxyConfig> = OnceLock::new();
    if let Some(config) = CELL.get() {
        return Ok(config);
    }
    let config = SlackFileProxyConfig::from_env()?;
    Ok(CELL.get_or_init(|| config))
}

impl SlackFileProxyConfig {
    fn from_env() -> Result<Self, ApiError> {
        let bot_token = non_empty_env("SLACK_BOT_TOKEN")
            .ok_or_else(|| ApiError::Internal("SLACK_BOT_TOKEN is not configured".to_owned()))?;
        Ok(Self {
            api_url: non_empty_env("SLACK_API_URL")
                .unwrap_or_else(|| DEFAULT_SLACK_API_URL.to_owned())
                .trim_end_matches('/')
                .to_owned(),
            bot_token,
            max_upload_bytes: positive_env_u64(
                "SLACK_FILE_PROXY_MAX_UPLOAD_BYTES",
                DEFAULT_MAX_UPLOAD_BYTES,
            ),
        })
    }
}

#[derive(Debug)]
struct SlackUploadTicket {
    upload_url: String,
    file_id: String,
}

async fn get_upload_url(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    filename: &str,
    length: u64,
    alt_txt: Option<&str>,
    snippet_type: Option<&str>,
) -> Result<SlackUploadTicket, ApiError> {
    let form = slack_get_upload_url_form(filename, length, alt_txt, snippet_type);
    let value = slack_api_post_form(client, config, "files.getUploadURLExternal", &form).await?;
    Ok(SlackUploadTicket {
        upload_url: required_slack_string(&value, "upload_url")?,
        file_id: required_slack_string(&value, "file_id")?,
    })
}

fn slack_get_upload_url_form(
    filename: &str,
    length: u64,
    alt_txt: Option<&str>,
    snippet_type: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut form = vec![
        ("filename", filename.to_owned()),
        ("length", length.to_string()),
        ("alt_txt", alt_txt.unwrap_or("").to_owned()),
        ("snippet_type", snippet_type.unwrap_or("").to_owned()),
    ];
    form.retain(|(_, value)| !value.is_empty());
    form
}

async fn upload_file_bytes(
    client: &reqwest::Client,
    upload_url: &str,
    body: Body,
    content_length: u64,
    content_type: Option<&str>,
) -> Result<(), ApiError> {
    let response = client
        .post(upload_url)
        .header(
            header::CONTENT_TYPE,
            content_type.unwrap_or("application/octet-stream"),
        )
        .header(header::CONTENT_LENGTH, content_length)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()))
        .send()
        .await
        .map_err(|error| ApiError::Internal(format!("Slack upload failed: {error}")))?;
    if !response.status().is_success() {
        return Err(ApiError::BadRequest(format!(
            "Slack upload failed with status {}",
            response.status().as_u16()
        )));
    }
    Ok(())
}

async fn complete_upload(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    file_id: &str,
    channel_id: &str,
    thread_ts: Option<&str>,
    title: &str,
    initial_comment: Option<&str>,
) -> Result<Value, ApiError> {
    let files = json!([{ "id": file_id, "title": title }]).to_string();
    let mut form = vec![
        ("files", files),
        ("channel_id", channel_id.to_owned()),
        ("thread_ts", thread_ts.unwrap_or("").to_owned()),
        ("initial_comment", initial_comment.unwrap_or("").to_owned()),
    ];
    form.retain(|(_, value)| !value.is_empty());
    let value = slack_api_post_form(client, config, "files.completeUploadExternal", &form).await?;
    value
        .get("files")
        .and_then(Value::as_array)
        .and_then(|files| files.first())
        .cloned()
        .ok_or_else(|| {
            ApiError::BadRequest("Slack upload response did not include file".to_owned())
        })
}

async fn slack_file_info(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    file_id: &str,
) -> Result<Value, ApiError> {
    let value = slack_api_post_form(
        client,
        config,
        "files.info",
        &[("file", file_id.to_owned())],
    )
    .await?;
    value.get("file").cloned().ok_or_else(|| {
        ApiError::BadRequest("Slack file info response did not include file".to_owned())
    })
}

async fn slack_api_post_form(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    method: &str,
    form: &[(&str, String)],
) -> Result<Value, ApiError> {
    let response = client
        .post(format!("{}/{}", config.api_url, method))
        .bearer_auth(&config.bot_token)
        .form(form)
        .send()
        .await
        .map_err(|error| ApiError::Internal(format!("Slack API request failed: {error}")))?;
    let status = response.status();
    let value = response
        .json::<Value>()
        .await
        .map_err(|error| ApiError::Internal(format!("Slack API response was not JSON: {error}")))?;
    if !status.is_success() || value.get("ok") != Some(&Value::Bool(true)) {
        let slack_error = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown_error");
        return Err(ApiError::BadRequest(format!(
            "Slack {method} failed: {slack_error}"
        )));
    }
    Ok(value)
}

fn authorize_slack_file_proxy(headers: &HeaderMap) -> Result<SlackFileProxyClaims, ApiError> {
    let token = bearer_token(headers)?;
    verify_console_jwt(token)
}

fn ensure_upload_channel_allowed(
    claims: &SlackFileProxyClaims,
    channel_id: &str,
) -> Result<(), ApiError> {
    ensure_channel_allowed(
        &claims.slack.upload_channels,
        channel_id,
        "JWT is not authorized to upload to this Slack channel",
    )
}

fn ensure_download_channel_allowed(
    claims: &SlackFileProxyClaims,
    channel_id: &str,
) -> Result<(), ApiError> {
    ensure_channel_allowed(
        &claims.slack.download_channels,
        channel_id,
        "JWT is not authorized to download from this Slack channel",
    )
}

fn ensure_channel_allowed(
    allowed_channels: &[String],
    channel_id: &str,
    message: &str,
) -> Result<(), ApiError> {
    if allowed_channels.iter().any(|allowed| allowed == channel_id) {
        return Ok(());
    }
    Err(ApiError::Forbidden(message.to_owned()))
}

fn slack_file_in_channel(file: &Value, channel_id: &str) -> bool {
    slack_file_channel_ids(file).contains(channel_id)
}

fn slack_file_channel_ids(file: &Value) -> BTreeSet<String> {
    let mut channels = BTreeSet::new();
    for key in ["channels", "groups", "ims"] {
        if let Some(values) = file.get(key).and_then(Value::as_array) {
            for value in values {
                if let Some(channel) = value.as_str() {
                    channels.insert(channel.to_owned());
                }
            }
        }
    }
    if let Some(shares) = file.get("shares").and_then(Value::as_object) {
        for share_type in shares.values().filter_map(Value::as_object) {
            for (channel, _shares) in share_type {
                channels.insert(channel.to_owned());
            }
        }
    }
    channels
}

fn required_slack_string(value: &Value, field: &str) -> Result<String, ApiError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ApiError::BadRequest(format!("Slack response missing {field}")))
}

fn content_length(headers: &HeaderMap) -> Result<u64, ApiError> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| ApiError::BadRequest("Content-Length header is required".to_owned()))
}

fn ensure_upload_size(len: u64, max: u64) -> Result<(), ApiError> {
    if len == 0 {
        return Err(ApiError::BadRequest(
            "file body must not be empty".to_owned(),
        ));
    }
    if len > max {
        return Err(ApiError::PayloadTooLarge(format!(
            "file body exceeds {max} byte limit"
        )));
    }
    Ok(())
}

fn validate_slack_channel_id(channel_id: &str) -> Result<(), ApiError> {
    if channel_id.len() >= 9
        && matches!(channel_id.as_bytes().first(), Some(b'C' | b'D' | b'G'))
        && channel_id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Ok(());
    }
    Err(ApiError::BadRequest("invalid Slack channel ID".to_owned()))
}

fn validate_slack_file_id(file_id: &str) -> Result<(), ApiError> {
    if file_id.len() >= 9
        && file_id.starts_with('F')
        && file_id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Ok(());
    }
    Err(ApiError::BadRequest("invalid Slack file ID".to_owned()))
}

fn validate_slack_thread_ts(thread_ts: &str) -> Result<(), ApiError> {
    let Some((seconds, micros)) = thread_ts.split_once('.') else {
        return Err(ApiError::BadRequest("invalid Slack thread_ts".to_owned()));
    };
    if !seconds.is_empty()
        && !micros.is_empty()
        && seconds.bytes().all(|byte| byte.is_ascii_digit())
        && micros.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Ok(());
    }
    Err(ApiError::BadRequest("invalid Slack thread_ts".to_owned()))
}

fn validate_filename(filename: &str) -> Result<(), ApiError> {
    let filename = filename.trim();
    if filename.is_empty() || filename.contains('/') || filename.contains('\\') {
        return Err(ApiError::BadRequest("invalid filename".to_owned()));
    }
    Ok(())
}

fn validate_content_type(content_type: &str) -> Result<(), ApiError> {
    if content_type.trim().is_empty() || content_type.parse::<HeaderValue>().is_err() {
        return Err(ApiError::BadRequest("invalid content_type".to_owned()));
    }
    Ok(())
}

fn content_disposition_filename(filename: &str) -> String {
    let sanitized = filename
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("attachment; filename=\"{sanitized}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

    fn test_jwt(secret: &[u8], claims: Value) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret),
        )
        .unwrap()
    }

    #[test]
    fn verifies_hs256_jwt_and_separate_slack_channel_claims() {
        let token = test_jwt(
            b"secret",
            json!({
                "iss": "centaur-console",
                "sub": "user_123",
                "aud": "centaur-api",
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
                "slack": {
                    "upload_channels": ["C123456789"],
                    "download_channels": ["C987654321"]
                }
            }),
        );
        let claims = crate::api_jwt::verify_hs256_jwt::<SlackFileProxyClaims>(
            &token,
            b"secret",
            "centaur-api",
            "centaur-console",
        )
        .unwrap();
        ensure_upload_channel_allowed(&claims, "C123456789").unwrap();
        ensure_download_channel_allowed(&claims, "C987654321").unwrap();
        assert!(matches!(
            ensure_upload_channel_allowed(&claims, "C987654321").unwrap_err(),
            ApiError::Forbidden(_)
        ));
        assert!(matches!(
            ensure_download_channel_allowed(&claims, "C123456789").unwrap_err(),
            ApiError::Forbidden(_)
        ));
    }

    #[test]
    fn rejects_invalid_jwt_signature() {
        let token = test_jwt(
            b"secret",
            json!({
                "iss": "centaur-console",
                "sub": "user_123",
                "aud": "centaur-api",
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
                "slack": {
                    "upload_channels": ["C123456789"],
                    "download_channels": ["C123456789"]
                }
            }),
        );
        assert!(matches!(
            crate::api_jwt::verify_hs256_jwt::<SlackFileProxyClaims>(
                &token,
                b"other-secret",
                "centaur-api",
                "centaur-console"
            )
            .unwrap_err(),
            ApiError::Unauthorized(_)
        ));
    }

    #[test]
    fn rejects_expired_jwt() {
        let token = test_jwt(
            b"secret",
            json!({
                "iss": "centaur-console",
                "sub": "user_123",
                "aud": "centaur-api",
                "iat": 1i64,
                "exp": 1i64,
                "slack": {
                    "upload_channels": ["C123456789"],
                    "download_channels": ["C123456789"]
                }
            }),
        );
        assert!(matches!(
            crate::api_jwt::verify_hs256_jwt::<SlackFileProxyClaims>(
                &token,
                b"secret",
                "centaur-api",
                "centaur-console"
            )
            .unwrap_err(),
            ApiError::Unauthorized(_)
        ));
    }

    #[test]
    fn rejects_wrong_jwt_audience() {
        let token = test_jwt(
            b"secret",
            json!({
                "iss": "centaur-console",
                "sub": "user_123",
                "aud": "other-api",
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
                "slack": {
                    "upload_channels": ["C123456789"],
                    "download_channels": ["C123456789"]
                }
            }),
        );
        assert!(matches!(
            crate::api_jwt::verify_hs256_jwt::<SlackFileProxyClaims>(
                &token,
                b"secret",
                "centaur-api",
                "centaur-console"
            )
            .unwrap_err(),
            ApiError::Unauthorized(_)
        ));
    }

    #[test]
    fn accepts_jwt_audience_array() {
        let token = test_jwt(
            b"secret",
            json!({
                "iss": "centaur-console",
                "sub": "user_123",
                "aud": ["other-api", "centaur-api"],
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
                "slack": {
                    "upload_channels": ["C123456789"],
                    "download_channels": ["C123456789"]
                }
            }),
        );
        let claims = crate::api_jwt::verify_hs256_jwt::<SlackFileProxyClaims>(
            &token,
            b"secret",
            "centaur-api",
            "centaur-console",
        )
        .unwrap();
        ensure_upload_channel_allowed(&claims, "C123456789").unwrap();
        ensure_download_channel_allowed(&claims, "C123456789").unwrap();
    }

    #[test]
    fn rejects_missing_standard_jwt_claims() {
        let token = test_jwt(
            b"secret",
            json!({
                "aud": "centaur-api",
                "exp": 4_102_444_800i64,
                "slack": {
                    "upload_channels": ["C123456789"],
                    "download_channels": ["C123456789"]
                }
            }),
        );
        assert!(matches!(
            crate::api_jwt::verify_hs256_jwt::<SlackFileProxyClaims>(
                &token,
                b"secret",
                "centaur-api",
                "centaur-console"
            )
            .unwrap_err(),
            ApiError::Unauthorized(_)
        ));
    }

    #[test]
    fn extracts_channels_from_file_metadata() {
        let file = json!({
            "channels": ["C111111111"],
            "groups": ["G111111111"],
            "ims": ["D111111111"],
            "shares": {
                "public": {
                    "C222222222": [{"ts": "1.000001"}]
                },
                "private": {
                    "G222222222": [{"ts": "1.000002"}]
                }
            }
        });
        let channels = slack_file_channel_ids(&file);
        assert!(channels.contains("C111111111"));
        assert!(channels.contains("G111111111"));
        assert!(channels.contains("D111111111"));
        assert!(channels.contains("C222222222"));
        assert!(channels.contains("G222222222"));
    }

    #[test]
    fn upload_requires_content_length() {
        let headers = HeaderMap::new();
        assert!(matches!(
            content_length(&headers).unwrap_err(),
            ApiError::BadRequest(_)
        ));

        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_LENGTH, "42".parse().unwrap());
        assert_eq!(content_length(&headers).unwrap(), 42);
    }

    #[test]
    fn rejects_wrong_jwt_issuer() {
        let token = test_jwt(
            b"secret",
            json!({
                "iss": "other-issuer",
                "sub": "user_123",
                "aud": "centaur-api",
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
                "slack": {
                    "upload_channels": ["C123456789"],
                    "download_channels": ["C123456789"]
                }
            }),
        );
        assert!(matches!(
            crate::api_jwt::verify_hs256_jwt::<SlackFileProxyClaims>(
                &token,
                b"secret",
                "centaur-api",
                "centaur-console"
            )
            .unwrap_err(),
            ApiError::Unauthorized(_)
        ));
    }

    #[test]
    fn detects_unexpected_html_download_body() {
        assert!(upstream_body_is_unexpected_html(
            Some("text/html; charset=utf-8"),
            Some("image/png"),
        ));
        assert!(upstream_body_is_unexpected_html(Some("TEXT/HTML"), None));
        assert!(!upstream_body_is_unexpected_html(
            Some("text/html"),
            Some("text/html"),
        ));
        assert!(!upstream_body_is_unexpected_html(
            Some("image/png"),
            Some("image/png"),
        ));
        assert!(!upstream_body_is_unexpected_html(None, Some("image/png")));
    }

    #[test]
    fn validates_content_type() {
        validate_content_type("application/pdf").unwrap();
        validate_content_type("text/plain; charset=utf-8").unwrap();
        for content_type in ["", " ", "a\nb", "a\rb", "a\0b"] {
            assert!(matches!(
                validate_content_type(content_type).unwrap_err(),
                ApiError::BadRequest(_)
            ));
        }
    }

    #[test]
    fn upload_url_form_includes_alt_text_and_snippet_type() {
        let form = slack_get_upload_url_form("notes.txt", 42, Some("Release notes"), Some("text"));
        assert_eq!(
            form,
            vec![
                ("filename", "notes.txt".to_owned()),
                ("length", "42".to_owned()),
                ("alt_txt", "Release notes".to_owned()),
                ("snippet_type", "text".to_owned()),
            ]
        );

        let form = slack_get_upload_url_form("notes.txt", 42, None, None);
        assert_eq!(
            form,
            vec![
                ("filename", "notes.txt".to_owned()),
                ("length", "42".to_owned()),
            ]
        );
    }
}
