use std::{collections::BTreeSet, env};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ApiError, routes::AppState};

const DEFAULT_SLACK_API_URL: &str = "https://slack.com/api";
const DEFAULT_API_JWT_AUDIENCE: &str = "centaur-api";
const DEFAULT_MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;
const JWT_CLOCK_SKEW_SECONDS: i64 = 30;

pub(crate) fn slack_file_proxy_router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/slack/files/upload",
            post(upload_slack_file).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/slack/files/{file_id}/download",
            get(download_slack_file),
        )
        .route("/api/slack/search/messages", get(search_slack_messages))
        .route("/api/slack/search/files", get(search_slack_files))
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
struct SlackSearchQuery {
    query: String,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    count: Option<u32>,
    #[serde(default)]
    highlight: Option<bool>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    sort_dir: Option<String>,
    #[serde(default)]
    team_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackFileProxyClaims {
    iat: i64,
    iss: String,
    slack: SlackProxyClaims,
    #[serde(default)]
    sub: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackProxyClaims {
    #[serde(default)]
    upload_channels: Vec<String>,
    #[serde(default)]
    download_channels: Vec<String>,
    #[serde(default)]
    search_channels: Vec<String>,
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
    let config = SlackFileProxyConfig::from_env()?;
    let content_length = content_length(&headers)?;
    ensure_upload_size(content_length, config.max_upload_bytes)?;
    let client = reqwest::Client::new();
    let upload_ticket = get_upload_url(
        &client,
        &config,
        &query.filename,
        content_length,
        query.alt_txt.as_deref(),
        query.snippet_type.as_deref(),
    )
    .await?;
    upload_file_bytes(
        &client,
        &upload_ticket.upload_url,
        body,
        content_length,
        query.content_type.as_deref(),
    )
    .await?;
    let file = complete_upload(
        &client,
        &config,
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

    let config = SlackFileProxyConfig::from_env()?;
    let client = reqwest::Client::new();
    let file = slack_file_info(&client, &config, &file_id).await?;
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

    let upstream_headers = upstream.headers().clone();
    let mut response = Body::from_stream(upstream.bytes_stream()).into_response();
    let headers = response.headers_mut();
    if let Some(value) = file
        .get("mimetype")
        .and_then(Value::as_str)
        .and_then(|value| value.parse().ok())
    {
        headers.insert(header::CONTENT_TYPE, value);
    }
    if let Some(value) = upstream_headers.get(header::CONTENT_LENGTH).cloned() {
        headers.insert(header::CONTENT_LENGTH, value);
    }
    if let Some(filename) = file
        .get("name")
        .or_else(|| file.get("title"))
        .and_then(Value::as_str)
        .map(content_disposition_filename)
        .and_then(|value| value.parse().ok())
    {
        headers.insert(header::CONTENT_DISPOSITION, filename);
    }
    Ok(response)
}

async fn search_slack_messages(
    headers: HeaderMap,
    Query(query): Query<SlackSearchQuery>,
) -> Result<Json<Value>, ApiError> {
    search_slack(headers, query, SlackSearchKind::Messages).await
}

async fn search_slack_files(
    headers: HeaderMap,
    Query(query): Query<SlackSearchQuery>,
) -> Result<Json<Value>, ApiError> {
    search_slack(headers, query, SlackSearchKind::Files).await
}

#[derive(Clone, Copy, Debug)]
enum SlackSearchKind {
    Messages,
    Files,
}

impl SlackSearchKind {
    fn method(self) -> &'static str {
        match self {
            Self::Messages => "search.messages",
            Self::Files => "search.files",
        }
    }

    fn result_key(self) -> &'static str {
        match self {
            Self::Messages => "messages",
            Self::Files => "files",
        }
    }
}

async fn search_slack(
    headers: HeaderMap,
    query: SlackSearchQuery,
    kind: SlackSearchKind,
) -> Result<Json<Value>, ApiError> {
    let claims = authorize_slack_file_proxy(&headers)?;
    let channels = search_channels(&claims, query.channel_id.as_deref())?;
    let config = SlackFileProxyConfig::from_env()?;
    let client = reqwest::Client::new();
    let form = slack_search_form(&query, &channels)?;
    let mut response = slack_api_get_form(&client, &config, kind.method(), &form).await?;
    filter_search_response(&mut response, kind, &channels);
    Ok(Json(response))
}

#[derive(Debug)]
struct SlackFileProxyConfig {
    api_url: String,
    bot_token: String,
    max_upload_bytes: u64,
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

async fn slack_api_get_form(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    method: &str,
    form: &[(&str, String)],
) -> Result<Value, ApiError> {
    let response = client
        .get(format!("{}/{}", config.api_url, method))
        .bearer_auth(&config.bot_token)
        .query(form)
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
    let secret = non_empty_env("CENTAUR_API_JWT_SECRET")
        .ok_or_else(|| ApiError::Internal("CENTAUR_API_JWT_SECRET is not configured".to_owned()))?;
    let audience = non_empty_env("CENTAUR_API_JWT_AUDIENCE")
        .unwrap_or_else(|| DEFAULT_API_JWT_AUDIENCE.to_owned());
    verify_hs256_jwt(token, secret.as_bytes(), &audience)
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::Unauthorized("missing bearer token".to_owned()))?;
    value
        .strip_prefix("Bearer ")
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| ApiError::Unauthorized("missing bearer token".to_owned()))
}

fn verify_hs256_jwt(
    token: &str,
    secret: &[u8],
    expected_audience: &str,
) -> Result<SlackFileProxyClaims, ApiError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = JWT_CLOCK_SKEW_SECONDS as u64;
    validation.validate_nbf = true;
    validation.set_audience(&[expected_audience]);
    validation.set_required_spec_claims(&["exp", "iss", "sub", "aud"]);
    let token_data =
        decode::<SlackFileProxyClaims>(token, &DecodingKey::from_secret(secret), &validation)
            .map_err(|_| ApiError::Unauthorized("invalid JWT".to_owned()))?;
    validate_claims(&token_data.claims)?;
    Ok(token_data.claims)
}

fn validate_claims(claims: &SlackFileProxyClaims) -> Result<(), ApiError> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if claims.iat > now + JWT_CLOCK_SKEW_SECONDS {
        return Err(ApiError::Unauthorized(
            "JWT issued-at is in the future".to_owned(),
        ));
    }
    if claims.iss.trim().is_empty() {
        return Err(ApiError::Unauthorized("JWT issuer is required".to_owned()));
    }
    if claims.sub.as_deref().unwrap_or_default().trim().is_empty() {
        return Err(ApiError::Unauthorized("JWT subject is required".to_owned()));
    }
    Ok(())
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

fn search_channels(
    claims: &SlackFileProxyClaims,
    requested_channel_id: Option<&str>,
) -> Result<Vec<String>, ApiError> {
    if let Some(channel_id) = requested_channel_id {
        validate_slack_channel_id(channel_id)?;
        ensure_channel_allowed(
            &claims.slack.search_channels,
            channel_id,
            "JWT is not authorized to search this Slack channel",
        )?;
        return Ok(vec![channel_id.to_owned()]);
    }

    let mut channels = claims.slack.search_channels.clone();
    channels.sort();
    channels.dedup();
    for channel in &channels {
        validate_slack_channel_id(channel)?;
    }
    if channels.is_empty() {
        return Err(ApiError::Forbidden(
            "JWT is not authorized to search any Slack channels".to_owned(),
        ));
    }
    Ok(channels)
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

fn slack_search_form(
    query: &SlackSearchQuery,
    channels: &[String],
) -> Result<Vec<(&'static str, String)>, ApiError> {
    let locked_query = locked_search_query(&query.query, channels)?;
    let mut form = vec![
        ("query", locked_query),
        (
            "count",
            query
                .count
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        (
            "highlight",
            query
                .highlight
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        (
            "page",
            query
                .page
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        ("cursor", query.cursor.clone().unwrap_or_default()),
        ("sort", query.sort.clone().unwrap_or_default()),
        ("sort_dir", query.sort_dir.clone().unwrap_or_default()),
        ("team_id", query.team_id.clone().unwrap_or_default()),
    ];
    form.retain(|(_, value)| !value.is_empty());
    Ok(form)
}

fn locked_search_query(query: &str, channels: &[String]) -> Result<String, ApiError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(ApiError::BadRequest("query must not be empty".to_owned()));
    }
    if channels.is_empty() {
        return Err(ApiError::Forbidden(
            "JWT is not authorized to search any Slack channels".to_owned(),
        ));
    }
    let filters = channels
        .iter()
        .map(|channel| format!("in:<#{channel}>"))
        .collect::<Vec<_>>();
    if filters.len() == 1 {
        Ok(format!("{query} {}", filters[0]))
    } else {
        Ok(format!("{query} ({})", filters.join(" OR ")))
    }
}

fn filter_search_response(response: &mut Value, kind: SlackSearchKind, channels: &[String]) {
    let allowed = channels.iter().cloned().collect::<BTreeSet<_>>();
    let Some(container) = response
        .get_mut(kind.result_key())
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let Some(matches) = container.get_mut("matches").and_then(Value::as_array_mut) else {
        return;
    };
    matches.retain(|entry| search_match_in_allowed_channels(entry, kind, &allowed));
    let filtered_count = u64::try_from(matches.len()).unwrap_or(u64::MAX);
    container.insert("total".to_owned(), json!(filtered_count));
    if let Some(pagination) = container
        .get_mut("pagination")
        .and_then(Value::as_object_mut)
    {
        pagination.insert("total_count".to_owned(), json!(filtered_count));
    }
    if let Some(paging) = container.get_mut("paging").and_then(Value::as_object_mut) {
        paging.insert("total".to_owned(), json!(filtered_count));
    }
}

fn search_match_in_allowed_channels(
    entry: &Value,
    kind: SlackSearchKind,
    allowed: &BTreeSet<String>,
) -> bool {
    match kind {
        SlackSearchKind::Messages => entry
            .get("channel")
            .and_then(|channel| channel.get("id"))
            .and_then(Value::as_str)
            .is_some_and(|channel_id| allowed.contains(channel_id)),
        SlackSearchKind::Files => slack_file_channel_ids(entry)
            .iter()
            .any(|channel_id| allowed.contains(channel_id)),
    }
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

fn non_empty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn positive_env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};

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
                    "download_channels": ["C987654321"],
                    "search_channels": ["G123456789"]
                }
            }),
        );
        let claims = verify_hs256_jwt(&token, b"secret", "centaur-api").unwrap();
        ensure_upload_channel_allowed(&claims, "C123456789").unwrap();
        ensure_download_channel_allowed(&claims, "C987654321").unwrap();
        assert_eq!(
            search_channels(&claims, Some("G123456789")).unwrap(),
            vec!["G123456789".to_owned()]
        );
        assert!(matches!(
            ensure_upload_channel_allowed(&claims, "C987654321").unwrap_err(),
            ApiError::Forbidden(_)
        ));
        assert!(matches!(
            ensure_download_channel_allowed(&claims, "C123456789").unwrap_err(),
            ApiError::Forbidden(_)
        ));
        assert!(matches!(
            search_channels(&claims, Some("C123456789")).unwrap_err(),
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
            verify_hs256_jwt(&token, b"other-secret", "centaur-api").unwrap_err(),
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
            verify_hs256_jwt(&token, b"secret", "centaur-api").unwrap_err(),
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
            verify_hs256_jwt(&token, b"secret", "centaur-api").unwrap_err(),
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
        let claims = verify_hs256_jwt(&token, b"secret", "centaur-api").unwrap();
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
            verify_hs256_jwt(&token, b"secret", "centaur-api").unwrap_err(),
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

    #[test]
    fn search_form_locks_query_to_allowed_channels() {
        let query = SlackSearchQuery {
            query: "release notes".to_owned(),
            channel_id: None,
            count: Some(10),
            highlight: Some(true),
            page: None,
            cursor: Some("cursor-1".to_owned()),
            sort: Some("timestamp".to_owned()),
            sort_dir: Some("desc".to_owned()),
            team_id: Some("T12345678".to_owned()),
        };
        let form =
            slack_search_form(&query, &["C123456789".to_owned(), "G123456789".to_owned()]).unwrap();
        assert_eq!(
            form,
            vec![
                (
                    "query",
                    "release notes (in:<#C123456789> OR in:<#G123456789>)".to_owned(),
                ),
                ("count", "10".to_owned()),
                ("highlight", "true".to_owned()),
                ("cursor", "cursor-1".to_owned()),
                ("sort", "timestamp".to_owned()),
                ("sort_dir", "desc".to_owned()),
                ("team_id", "T12345678".to_owned()),
            ]
        );
    }

    #[test]
    fn search_channels_uses_requested_channel_or_full_claim_list() {
        let claims = SlackFileProxyClaims {
            iat: 1,
            iss: "centaur-console".to_owned(),
            slack: SlackProxyClaims {
                upload_channels: vec![],
                download_channels: vec![],
                search_channels: vec![
                    "G123456789".to_owned(),
                    "C123456789".to_owned(),
                    "C123456789".to_owned(),
                ],
            },
            sub: Some("user_123".to_owned()),
        };

        assert_eq!(
            search_channels(&claims, Some("G123456789")).unwrap(),
            vec!["G123456789".to_owned()]
        );
        assert_eq!(
            search_channels(&claims, None).unwrap(),
            vec!["C123456789".to_owned(), "G123456789".to_owned()]
        );
        assert!(matches!(
            search_channels(&claims, Some("D123456789")).unwrap_err(),
            ApiError::Forbidden(_)
        ));
    }

    #[test]
    fn search_response_filter_removes_disallowed_message_matches() {
        let mut response = json!({
            "ok": true,
            "messages": {
                "matches": [
                    {"channel": {"id": "C123456789"}, "text": "allowed"},
                    {"channel": {"id": "C987654321"}, "text": "denied"}
                ],
                "pagination": {"total_count": 2},
                "paging": {"total": 2},
                "total": 2
            }
        });

        filter_search_response(
            &mut response,
            SlackSearchKind::Messages,
            &["C123456789".to_owned()],
        );

        assert_eq!(response["messages"]["matches"].as_array().unwrap().len(), 1);
        assert_eq!(response["messages"]["matches"][0]["text"], "allowed");
        assert_eq!(response["messages"]["total"], json!(1));
        assert_eq!(response["messages"]["pagination"]["total_count"], json!(1));
        assert_eq!(response["messages"]["paging"]["total"], json!(1));
    }

    #[test]
    fn search_response_filter_removes_disallowed_file_matches() {
        let mut response = json!({
            "ok": true,
            "files": {
                "matches": [
                    {"id": "F123456789", "channels": ["C123456789"]},
                    {"id": "F987654321", "groups": ["G987654321"]}
                ],
                "pagination": {"total_count": 2},
                "paging": {"total": 2},
                "total": 2
            }
        });

        filter_search_response(
            &mut response,
            SlackSearchKind::Files,
            &["C123456789".to_owned()],
        );

        assert_eq!(response["files"]["matches"].as_array().unwrap().len(), 1);
        assert_eq!(response["files"]["matches"][0]["id"], "F123456789");
        assert_eq!(response["files"]["total"], json!(1));
    }
}
