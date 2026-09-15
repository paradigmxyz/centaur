use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
    time::{Duration, Instant},
};

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
const DEFAULT_SLACK_FILES_LIST_LIMIT: u16 = 100;
const MAX_SLACK_FILES_LIST_LIMIT: u16 = 200;
const DEFAULT_SLACK_CHANNELS_LIMIT: u16 = 100;
const MAX_SLACK_CHANNELS_LIMIT: u16 = 200;
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(60);
const SLACK_CHANNEL_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const SLACK_CHANNEL_REFRESH_RETRY_DELAY: Duration = Duration::from_secs(60);
const SLACK_RATE_LIMIT_MAX_RETRIES: u32 = 2;
const SLACK_RATE_LIMIT_MAX_DELAY: Duration = Duration::from_secs(60);

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
        .route("/api/slack/files", get(get_slack_files))
        .route(
            "/api/slack/files/upload",
            post(upload_slack_file).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/slack/files/{file_id}/download",
            get(download_slack_file),
        )
        .route("/api/slack/files/{file_id}/info", get(get_slack_file_info))
        .route("/api/slack/channels", get(get_slack_channels))
        .route(
            "/api/slack/channels/{channel_id}/history",
            get(get_slack_channel_history),
        )
        .route(
            "/api/slack/channels/{channel_id}/members",
            get(get_slack_channel_members),
        )
        .route(
            "/api/slack/channels/{channel_id}/threads/{thread_ts}/replies",
            get(get_slack_thread_replies),
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
struct SlackFileInfoQuery {
    channel_id: String,
}

#[derive(Debug, Deserialize)]
struct SlackFilesListQuery {
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    limit: Option<u16>,
    #[serde(default)]
    page: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SlackChannelsQuery {
    #[serde(default)]
    limit: Option<u16>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    history_only: bool,
}

#[derive(Debug, Deserialize)]
struct SlackChannelHistoryQuery {
    #[serde(default)]
    latest: Option<String>,
    #[serde(default)]
    oldest: Option<String>,
    #[serde(default)]
    inclusive: Option<bool>,
    #[serde(default)]
    include_all_metadata: Option<bool>,
    #[serde(default)]
    limit: Option<u16>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackChannelMembersQuery {
    #[serde(default)]
    limit: Option<u16>,
    #[serde(default)]
    cursor: Option<String>,
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
    #[serde(default)]
    history_channels: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SlackFileUploadResponse {
    ok: bool,
    file_id: String,
    channel_id: String,
    thread_ts: Option<String>,
    file: Value,
}

#[derive(Debug, Serialize)]
struct SlackChannelsResponse {
    ok: bool,
    channels: Vec<SlackChannelItem>,
    count: usize,
    total_count: usize,
    response_metadata: SlackChannelsResponseMetadata,
}

#[derive(Debug, Serialize)]
struct SlackChannelsResponseMetadata {
    next_cursor: String,
}

#[derive(Debug, Serialize)]
struct SlackFilesListResponse {
    ok: bool,
    files: Vec<Value>,
    count: usize,
    page: u32,
    paging: Option<Value>,
    has_more: bool,
}

#[derive(Debug, Serialize)]
struct SlackFileInfoResponse {
    ok: bool,
    file_id: String,
    channel_id: String,
    file: Value,
}

#[derive(Debug, Serialize)]
struct SlackChannelItem {
    id: String,
    name: String,
    purpose: String,
    topic: String,
    member_count: u64,
    is_private: bool,
    is_member: bool,
    can_upload: bool,
    can_download: bool,
    can_read_history: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct SlackChannel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    purpose: SlackChannelText,
    #[serde(default)]
    topic: SlackChannelText,
    #[serde(default)]
    num_members: u64,
    #[serde(default)]
    is_private: Option<bool>,
    #[serde(default)]
    is_member: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct SlackChannelText {
    #[serde(default)]
    value: String,
}

#[derive(Debug, Deserialize)]
struct SlackChannelInfoResponse {
    channel: SlackChannel,
}

#[derive(Debug, Deserialize)]
struct SlackChannelsPage {
    channels: Vec<SlackChannel>,
    #[serde(default)]
    response_metadata: SlackResponseMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct SlackResponseMetadata {
    #[serde(default)]
    next_cursor: String,
}

async fn upload_slack_file(
    headers: HeaderMap,
    Query(query): Query<SlackFileUploadQuery>,
    body: Body,
) -> Result<Json<SlackFileUploadResponse>, ApiError> {
    let claims = authorize_slack_file_proxy(&headers)?;
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
    ensure_slack_channel_allowed(
        &claims,
        client,
        config,
        &query.channel_id,
        SlackChannelPermission::Upload,
    )
    .await?;
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
    let client = http_client();
    let (config, file) =
        authorized_slack_file_info(&headers, client, &file_id, &query.channel_id).await?;
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

async fn get_slack_files(
    headers: HeaderMap,
    Query(query): Query<SlackFilesListQuery>,
) -> Result<Json<SlackFilesListResponse>, ApiError> {
    let claims = authorize_slack_file_proxy(&headers)?;
    validate_slack_files_list_query(&query)?;
    let channel_id = query
        .channel_id
        .as_deref()
        .expect("validate_slack_files_list_query requires channel_id");
    let effective_page = slack_files_list_page(&query);

    let config = slack_proxy_config()?;
    let client = http_client();
    ensure_slack_channel_allowed(
        &claims,
        client,
        config,
        channel_id,
        SlackChannelPermission::Download,
    )
    .await?;
    let mut value = slack_files_list(client, config, channel_id, &query).await?;
    let mut files = Vec::new();
    let mut seen_file_ids = BTreeSet::new();
    let paging = value.get("paging").cloned();
    let has_more = slack_files_list_has_more(&value);
    let file_values = value
        .get_mut("files")
        .and_then(Value::as_array_mut)
        .map(std::mem::take)
        .unwrap_or_default();
    for file in file_values {
        let Some(file_id) = file.get("id").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        if seen_file_ids.insert(file_id) {
            files.push(file);
        }
    }
    files.sort_by(|left, right| {
        slack_file_created(right)
            .cmp(&slack_file_created(left))
            .then_with(|| slack_file_id(left).cmp(slack_file_id(right)))
    });

    Ok(Json(SlackFilesListResponse {
        ok: true,
        count: files.len(),
        files,
        page: effective_page,
        paging,
        has_more,
    }))
}

async fn get_slack_file_info(
    headers: HeaderMap,
    Path(file_id): Path<String>,
    Query(query): Query<SlackFileInfoQuery>,
) -> Result<Json<SlackFileInfoResponse>, ApiError> {
    let (_, file) =
        authorized_slack_file_info(&headers, http_client(), &file_id, &query.channel_id).await?;

    Ok(Json(SlackFileInfoResponse {
        ok: true,
        file_id,
        channel_id: query.channel_id,
        file,
    }))
}

async fn authorized_slack_file_info(
    headers: &HeaderMap,
    client: &reqwest::Client,
    file_id: &str,
    channel_id: &str,
) -> Result<(&'static SlackFileProxyConfig, Value), ApiError> {
    let claims = authorize_slack_file_proxy(headers)?;
    validate_slack_channel_id(channel_id)?;
    validate_slack_file_id(file_id)?;

    let config = slack_proxy_config()?;
    ensure_slack_channel_allowed(
        &claims,
        client,
        config,
        channel_id,
        SlackChannelPermission::Download,
    )
    .await?;
    let file = slack_file_info(client, config, file_id).await?;
    if !slack_file_in_channel(&file, channel_id) {
        return Err(ApiError::Forbidden(
            "file is not shared in an allowed Slack channel".to_owned(),
        ));
    }
    Ok((config, file))
}

async fn get_slack_channels(
    headers: HeaderMap,
    Query(query): Query<SlackChannelsQuery>,
) -> Result<Json<SlackChannelsResponse>, ApiError> {
    let claims = authorize_slack_file_proxy(&headers)?;
    let channel_ids = slack_channel_ids_from_claims(&claims)?;
    let (limit, offset) = validate_slack_channels_query(&query)?;

    let config = slack_proxy_config()?;
    let client = http_client();
    let mut channels_by_id = BTreeMap::new();
    match slack_public_channels().await {
        Ok(channels) => {
            for channel in channels {
                if slack_channel_has_default_access(&channel) {
                    channels_by_id.insert(channel.id.clone(), channel);
                }
            }
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                "listing explicitly granted Slack channels without public channel metadata"
            );
        }
    }
    for channel_id in channel_ids {
        if channels_by_id.contains_key(&channel_id) {
            continue;
        }
        match slack_channel_info(client, config, &channel_id).await {
            Ok(channel) => {
                channels_by_id.insert(channel_id, channel);
            }
            Err(error) => {
                tracing::warn!(
                    channel_id,
                    error = %error,
                    "skipping Slack channel whose metadata could not be fetched"
                );
            }
        }
    }
    let channels = channels_by_id
        .into_iter()
        .map(|(channel_id, channel)| slack_channel_item(&claims, &channel_id, &channel))
        .collect::<Vec<_>>();
    let (channels, total_count, next_cursor) =
        paginate_slack_channels(channels, &query, limit, offset);

    Ok(Json(SlackChannelsResponse {
        ok: true,
        count: channels.len(),
        total_count,
        channels,
        response_metadata: SlackChannelsResponseMetadata { next_cursor },
    }))
}

fn paginate_slack_channels(
    mut channels: Vec<SlackChannelItem>,
    query: &SlackChannelsQuery,
    limit: usize,
    offset: usize,
) -> (Vec<SlackChannelItem>, usize, String) {
    if query.history_only {
        channels.retain(|channel| channel.can_read_history);
    }
    if let Some(search) = query
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    {
        let search = search.to_ascii_lowercase();
        channels.retain(|channel| channel.name.to_ascii_lowercase().contains(&search));
    }
    channels.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });

    let total_count = channels.len();
    let channels = channels
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let next_offset = offset.saturating_add(channels.len());
    let next_cursor = if next_offset < total_count {
        next_offset.to_string()
    } else {
        String::new()
    };
    (channels, total_count, next_cursor)
}

async fn get_slack_channel_history(
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Query(query): Query<SlackChannelHistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    let claims = authorize_slack_file_proxy(&headers)?;
    validate_slack_channel_id(&channel_id)?;
    validate_slack_channel_history_query(&query)?;

    let config = slack_proxy_config()?;
    let client = http_client();
    ensure_slack_channel_allowed(
        &claims,
        client,
        config,
        &channel_id,
        SlackChannelPermission::History,
    )
    .await?;
    let value = slack_channel_history(client, config, &channel_id, &query).await?;
    Ok(Json(value))
}

async fn get_slack_channel_members(
    headers: HeaderMap,
    Path(channel_id): Path<String>,
    Query(query): Query<SlackChannelMembersQuery>,
) -> Result<Json<Value>, ApiError> {
    let claims = authorize_slack_file_proxy(&headers)?;
    validate_slack_channel_id(&channel_id)?;
    validate_slack_channel_members_query(&query)?;

    let config = slack_proxy_config()?;
    let client = http_client();
    ensure_slack_channel_allowed(
        &claims,
        client,
        config,
        &channel_id,
        SlackChannelPermission::History,
    )
    .await?;
    let value = slack_channel_members(client, config, &channel_id, &query).await?;
    Ok(Json(value))
}

async fn get_slack_thread_replies(
    headers: HeaderMap,
    Path((channel_id, thread_ts)): Path<(String, String)>,
    Query(query): Query<SlackChannelHistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    let claims = authorize_slack_file_proxy(&headers)?;
    validate_slack_channel_id(&channel_id)?;
    validate_slack_thread_ts(&thread_ts)?;
    validate_slack_channel_history_query(&query)?;

    let config = slack_proxy_config()?;
    let client = http_client();
    ensure_slack_channel_allowed(
        &claims,
        client,
        config,
        &channel_id,
        SlackChannelPermission::History,
    )
    .await?;
    let value = slack_thread_replies(client, config, &channel_id, &thread_ts, &query).await?;
    Ok(Json(value))
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

struct CachedSlackValue<T> {
    fetched_at: Instant,
    value: T,
}

impl<T: Clone> CachedSlackValue<T> {
    fn new(fetched_at: Instant, value: T) -> Self {
        Self { fetched_at, value }
    }

    fn get_if_fresh(&self, now: Instant) -> Option<T> {
        (now.saturating_duration_since(self.fetched_at) < SLACK_CHANNEL_CACHE_TTL)
            .then(|| self.value.clone())
    }
}

type SlackChannelInfoCache = tokio::sync::Mutex<BTreeMap<String, CachedSlackValue<SlackChannel>>>;
type SlackPublicChannelCache = tokio::sync::Mutex<SlackPublicChannelCacheState>;

#[derive(Default)]
struct SlackPublicChannelCacheState {
    channels: Option<CachedSlackValue<Vec<SlackChannel>>>,
    retry_refresh_at: Option<Instant>,
    refreshing: bool,
}

impl SlackPublicChannelCacheState {
    fn fresh_channels(&self, now: Instant) -> Option<Vec<SlackChannel>> {
        self.channels
            .as_ref()
            .and_then(|channels| channels.get_if_fresh(now))
    }

    fn stale_channels(&self) -> Option<Vec<SlackChannel>> {
        self.channels.as_ref().map(|entry| entry.value.clone())
    }

    fn can_refresh(&self, now: Instant) -> bool {
        !self.refreshing && !self.retry_refresh_at.is_some_and(|retry_at| retry_at > now)
    }

    fn mark_refresh_failed(&mut self, now: Instant) {
        self.refreshing = false;
        self.retry_refresh_at = Some(now + SLACK_CHANNEL_REFRESH_RETRY_DELAY);
    }
}

fn slack_channel_info_cache() -> &'static SlackChannelInfoCache {
    static CACHE: OnceLock<SlackChannelInfoCache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn slack_public_channel_cache() -> &'static SlackPublicChannelCache {
    static CACHE: OnceLock<SlackPublicChannelCache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
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
    let value =
        slack_api_post_form(client, config, "files.info", &slack_file_info_form(file_id)).await?;
    value.get("file").cloned().ok_or_else(|| {
        ApiError::BadRequest("Slack file info response did not include file".to_owned())
    })
}

async fn slack_channel_info(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    channel_id: &str,
) -> Result<SlackChannel, ApiError> {
    {
        let entries = slack_channel_info_cache().lock().await;
        let now = Instant::now();
        if let Some(channel) = entries
            .get(channel_id)
            .and_then(|entry| entry.get_if_fresh(now))
        {
            return Ok(channel);
        }
    }

    let channel = fetch_slack_channel_info(client, config, channel_id).await?;
    slack_channel_info_cache().lock().await.insert(
        channel_id.to_owned(),
        CachedSlackValue::new(Instant::now(), channel.clone()),
    );
    Ok(channel)
}

async fn fetch_slack_channel_info(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    channel_id: &str,
) -> Result<SlackChannel, ApiError> {
    let value = slack_api_post_form(
        client,
        config,
        "conversations.info",
        &slack_channel_info_form(channel_id),
    )
    .await?;
    serde_json::from_value::<SlackChannelInfoResponse>(value)
        .map(|response| response.channel)
        .map_err(|error| {
            ApiError::Internal(format!(
                "Slack conversations.info response was invalid: {error}"
            ))
        })
}

fn slack_channel_info_form(channel_id: &str) -> Vec<(&'static str, String)> {
    vec![
        ("channel", channel_id.to_owned()),
        ("include_num_members", "true".to_owned()),
    ]
}

pub(crate) fn warm_slack_public_channel_cache() {
    if slack_proxy_config().is_err() {
        return;
    }
    tokio::spawn(async {
        match slack_public_channels().await {
            Ok(channels) => tracing::info!(
                channel_count = channels.len(),
                "warmed Slack public channel cache"
            ),
            Err(error) => tracing::warn!(
                error = %error,
                "failed to warm Slack public channel cache"
            ),
        }
    });
}

async fn slack_public_channels() -> Result<Vec<SlackChannel>, ApiError> {
    let mut cache = slack_public_channel_cache().lock().await;
    let now = Instant::now();
    if let Some(channels) = cache.fresh_channels(now) {
        return Ok(channels);
    }
    if let Some(stale) = cache.stale_channels() {
        if cache.can_refresh(now) {
            cache.refreshing = true;
            drop(cache);
            spawn_slack_public_channel_refresh();
        }
        return Ok(stale);
    }
    if !cache.can_refresh(now) {
        return Ok(Vec::new());
    }
    cache.refreshing = true;
    drop(cache);

    match spawn_slack_public_channel_refresh().await {
        Ok(result) => result,
        Err(error) => {
            let mut cache = slack_public_channel_cache().lock().await;
            cache.mark_refresh_failed(Instant::now());
            Err(ApiError::Internal(format!(
                "Slack channel refresh monitor failed: {error}"
            )))
        }
    }
}

fn spawn_slack_public_channel_refresh()
-> tokio::task::JoinHandle<Result<Vec<SlackChannel>, ApiError>> {
    let refresh = tokio::spawn(async {
        let result = match slack_proxy_config() {
            Ok(config) => fetch_slack_public_channels(http_client(), config).await,
            Err(error) => Err(error),
        };
        complete_slack_public_channel_refresh(result).await
    });
    tokio::spawn(await_slack_public_channel_refresh(
        refresh,
        slack_public_channel_cache(),
    ))
}

async fn await_slack_public_channel_refresh(
    refresh: tokio::task::JoinHandle<Result<Vec<SlackChannel>, ApiError>>,
    cache: &SlackPublicChannelCache,
) -> Result<Vec<SlackChannel>, ApiError> {
    match refresh.await {
        Ok(result) => {
            if let Err(error) = &result {
                tracing::warn!(error = %error, "Slack channel cache refresh failed");
            }
            result
        }
        Err(error) => {
            tracing::warn!(error = %error, "Slack channel cache refresh task failed");
            let mut cache = cache.lock().await;
            cache.mark_refresh_failed(Instant::now());
            Err(ApiError::Internal(format!(
                "Slack channel refresh task failed: {error}"
            )))
        }
    }
}

async fn complete_slack_public_channel_refresh(
    result: Result<Vec<SlackChannel>, ApiError>,
) -> Result<Vec<SlackChannel>, ApiError> {
    let channels = match result {
        Ok(channels) => channels,
        Err(error) => {
            let mut cache = slack_public_channel_cache().lock().await;
            cache.mark_refresh_failed(Instant::now());
            return Err(error);
        }
    };
    let fetched_at = Instant::now();
    {
        let mut cache = slack_public_channel_cache().lock().await;
        cache.channels = Some(CachedSlackValue::new(fetched_at, channels.clone()));
        cache.retry_refresh_at = None;
        cache.refreshing = false;
    }
    {
        let mut channel_info = slack_channel_info_cache().lock().await;
        for channel in &channels {
            channel_info.insert(
                channel.id.clone(),
                CachedSlackValue::new(fetched_at, channel.clone()),
            );
        }
    }
    Ok(channels)
}

async fn fetch_slack_public_channels(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
) -> Result<Vec<SlackChannel>, ApiError> {
    let mut channels = Vec::new();
    let mut cursor = String::new();
    let mut seen_cursors = BTreeSet::new();
    loop {
        let form = slack_public_channels_form(&cursor);
        let value = slack_api_post_form(client, config, "users.conversations", &form).await?;
        let mut page = serde_json::from_value::<SlackChannelsPage>(value).map_err(|error| {
            ApiError::Internal(format!(
                "Slack users.conversations response was invalid: {error}"
            ))
        })?;
        // users.conversations only returns channels the token's bot belongs to.
        for channel in &mut page.channels {
            channel.is_member = true;
        }
        channels.extend(page.channels);
        let next_cursor = page.response_metadata.next_cursor;
        if next_cursor.is_empty() {
            break;
        }
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err(ApiError::Internal(
                "Slack users.conversations repeated a pagination cursor".to_owned(),
            ));
        }
        cursor = next_cursor;
    }
    Ok(channels)
}

fn slack_public_channels_form(cursor: &str) -> Vec<(&'static str, String)> {
    let mut form = vec![
        ("types", "public_channel".to_owned()),
        ("exclude_archived", "true".to_owned()),
        ("limit", "200".to_owned()),
    ];
    if !cursor.is_empty() {
        form.push(("cursor", cursor.to_owned()));
    }
    form
}

async fn slack_channel_history(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    channel_id: &str,
    query: &SlackChannelHistoryQuery,
) -> Result<Value, ApiError> {
    let form = slack_channel_history_form(channel_id, query);
    slack_api_post_form(client, config, "conversations.history", &form).await
}

async fn slack_thread_replies(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    channel_id: &str,
    thread_ts: &str,
    query: &SlackChannelHistoryQuery,
) -> Result<Value, ApiError> {
    let form = slack_thread_replies_form(channel_id, thread_ts, query);
    slack_api_post_form(client, config, "conversations.replies", &form).await
}

async fn slack_files_list(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    channel_id: &str,
    query: &SlackFilesListQuery,
) -> Result<Value, ApiError> {
    let form = slack_files_list_form(channel_id, query);
    slack_api_post_form(client, config, "files.list", &form).await
}

async fn slack_channel_members(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    channel_id: &str,
    query: &SlackChannelMembersQuery,
) -> Result<Value, ApiError> {
    let form = slack_channel_members_form(channel_id, query);
    slack_api_post_form(client, config, "conversations.members", &form).await
}

fn slack_files_list_form(
    channel_id: &str,
    query: &SlackFilesListQuery,
) -> Vec<(&'static str, String)> {
    vec![
        ("channel", channel_id.to_owned()),
        ("count", slack_files_list_limit(query).to_string()),
        ("page", slack_files_list_page(query).to_string()),
    ]
}

fn slack_channel_members_form(
    channel_id: &str,
    query: &SlackChannelMembersQuery,
) -> Vec<(&'static str, String)> {
    let mut form = vec![
        ("channel", channel_id.to_owned()),
        (
            "limit",
            query
                .limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        ("cursor", query.cursor.clone().unwrap_or_default()),
    ];
    form.retain(|(_, value)| !value.is_empty());
    form
}

fn slack_file_info_form(file_id: &str) -> Vec<(&'static str, String)> {
    vec![("file", file_id.to_owned())]
}

fn slack_channel_history_form(
    channel_id: &str,
    query: &SlackChannelHistoryQuery,
) -> Vec<(&'static str, String)> {
    let mut form = vec![
        ("channel", channel_id.to_owned()),
        ("latest", query.latest.clone().unwrap_or_default()),
        ("oldest", query.oldest.clone().unwrap_or_default()),
        (
            "inclusive",
            query
                .inclusive
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        (
            "include_all_metadata",
            query
                .include_all_metadata
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        (
            "limit",
            query
                .limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ),
        ("cursor", query.cursor.clone().unwrap_or_default()),
    ];
    form.retain(|(_, value)| !value.is_empty());
    form
}

fn slack_thread_replies_form(
    channel_id: &str,
    thread_ts: &str,
    query: &SlackChannelHistoryQuery,
) -> Vec<(&'static str, String)> {
    let mut form = slack_channel_history_form(channel_id, query);
    form.push(("ts", thread_ts.to_owned()));
    form
}

fn slack_files_list_limit(query: &SlackFilesListQuery) -> u16 {
    query.limit.unwrap_or(DEFAULT_SLACK_FILES_LIST_LIMIT)
}

fn slack_files_list_page(query: &SlackFilesListQuery) -> u32 {
    query.page.unwrap_or(1)
}

fn slack_files_list_has_more(value: &Value) -> bool {
    value
        .get("paging")
        .is_some_and(|paging| slack_paging_page(paging) < slack_paging_pages(paging))
}

fn slack_paging_page(paging: &Value) -> u64 {
    paging
        .get("page")
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn slack_paging_pages(paging: &Value) -> u64 {
    paging
        .get("pages")
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

async fn slack_api_post_form(
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    method: &str,
    form: &[(&str, String)],
) -> Result<Value, ApiError> {
    let mut retry_count = 0;
    loop {
        let response = client
            .post(format!("{}/{}", config.api_url, method))
            .bearer_auth(&config.bot_token)
            .form(form)
            .send()
            .await
            .map_err(|error| ApiError::Internal(format!("Slack API request failed: {error}")))?;
        let status = response.status();
        if status.as_u16() == 429 {
            if retry_count >= SLACK_RATE_LIMIT_MAX_RETRIES {
                return Err(ApiError::ServiceUnavailable(format!(
                    "Slack {method} remained rate limited after retries"
                )));
            }
            let retry_after = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            let delay = slack_rate_limit_retry_delay(retry_count, retry_after);
            tracing::warn!(
                method,
                retry_count,
                retry_after_ms = delay.as_millis(),
                "Slack API rate limited; retrying"
            );
            tokio::time::sleep(delay).await;
            retry_count += 1;
            continue;
        }

        let value = response.json::<Value>().await.map_err(|error| {
            ApiError::Internal(format!("Slack API response was not JSON: {error}"))
        })?;
        if !status.is_success() || value.get("ok") != Some(&Value::Bool(true)) {
            let slack_error = value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown_error");
            return Err(ApiError::BadRequest(format!(
                "Slack {method} failed: {slack_error}"
            )));
        }
        return Ok(value);
    }
}

fn slack_rate_limit_retry_delay(retry_count: u32, retry_after: Option<u64>) -> Duration {
    let seconds = retry_after.unwrap_or_else(|| 1_u64 << retry_count.min(6));
    Duration::from_secs(seconds).min(SLACK_RATE_LIMIT_MAX_DELAY)
}

fn authorize_slack_file_proxy(headers: &HeaderMap) -> Result<SlackFileProxyClaims, ApiError> {
    let token = bearer_token(headers)?;
    verify_console_jwt(token)
}

enum SlackChannelPermission {
    Upload,
    Download,
    History,
}

impl SlackChannelPermission {
    fn allowed_channels<'a>(&self, claims: &'a SlackFileProxyClaims) -> &'a [String] {
        match self {
            Self::Upload => &claims.slack.upload_channels,
            Self::Download => &claims.slack.download_channels,
            Self::History => &claims.slack.history_channels,
        }
    }

    fn forbidden_message(&self) -> &'static str {
        match self {
            Self::Upload => "JWT is not authorized to upload to this Slack channel",
            Self::Download => "JWT is not authorized to download from this Slack channel",
            Self::History => "JWT is not authorized to read history from this Slack channel",
        }
    }
}

async fn ensure_slack_channel_allowed(
    claims: &SlackFileProxyClaims,
    client: &reqwest::Client,
    config: &SlackFileProxyConfig,
    channel_id: &str,
    permission: SlackChannelPermission,
) -> Result<(), ApiError> {
    if permission
        .allowed_channels(claims)
        .iter()
        .any(|allowed| allowed == channel_id)
    {
        return Ok(());
    }

    let channel = slack_channel_info(client, config, channel_id).await?;
    if slack_channel_has_default_access(&channel) {
        return Ok(());
    }

    Err(ApiError::Forbidden(
        permission.forbidden_message().to_owned(),
    ))
}

fn slack_channel_has_default_access(channel: &SlackChannel) -> bool {
    channel.is_private == Some(false) && channel.is_member
}

fn slack_channel_ids_from_claims(claims: &SlackFileProxyClaims) -> Result<Vec<String>, ApiError> {
    validated_channel_ids(
        claims
            .slack
            .upload_channels
            .iter()
            .chain(claims.slack.download_channels.iter())
            .chain(claims.slack.history_channels.iter()),
    )
}

fn validated_channel_ids<'a>(
    raw_channel_ids: impl IntoIterator<Item = &'a String>,
) -> Result<Vec<String>, ApiError> {
    let mut channel_ids: BTreeSet<String> = BTreeSet::new();
    for channel_id in raw_channel_ids {
        validate_slack_channel_id(channel_id)?;
        channel_ids.insert(channel_id.to_owned());
    }
    Ok(channel_ids.into_iter().collect())
}

fn slack_channel_item(
    claims: &SlackFileProxyClaims,
    channel_id: &str,
    channel: &SlackChannel,
) -> SlackChannelItem {
    SlackChannelItem {
        id: channel_id.to_owned(),
        name: if channel.name.is_empty() {
            channel_id.to_owned()
        } else {
            channel.name.clone()
        },
        purpose: channel.purpose.value.clone(),
        topic: channel.topic.value.clone(),
        member_count: channel.num_members,
        is_private: channel
            .is_private
            .unwrap_or_else(|| channel_id.starts_with('G')),
        is_member: channel.is_member,
        can_upload: claims
            .slack
            .upload_channels
            .iter()
            .any(|allowed| allowed == channel_id)
            || slack_channel_has_default_access(channel),
        can_download: claims
            .slack
            .download_channels
            .iter()
            .any(|allowed| allowed == channel_id)
            || slack_channel_has_default_access(channel),
        can_read_history: claims
            .slack
            .history_channels
            .iter()
            .any(|allowed| allowed == channel_id)
            || slack_channel_has_default_access(channel),
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

fn slack_file_created(file: &Value) -> u64 {
    file.get("created")
        .and_then(Value::as_u64)
        .unwrap_or_default()
}

fn slack_file_id(file: &Value) -> &str {
    file.get("id").and_then(Value::as_str).unwrap_or_default()
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

fn validate_slack_channels_query(query: &SlackChannelsQuery) -> Result<(usize, usize), ApiError> {
    let limit = query.limit.unwrap_or(DEFAULT_SLACK_CHANNELS_LIMIT);
    if !(1..=MAX_SLACK_CHANNELS_LIMIT).contains(&limit) {
        return Err(ApiError::BadRequest(format!(
            "Slack channels limit must be between 1 and {MAX_SLACK_CHANNELS_LIMIT}"
        )));
    }
    let offset = match query.cursor.as_deref() {
        Some(cursor) if !cursor.is_empty() => cursor
            .parse::<usize>()
            .map_err(|_| ApiError::BadRequest("invalid Slack channels cursor".to_owned()))?,
        _ => 0,
    };
    Ok((usize::from(limit), offset))
}

fn validate_slack_channel_history_query(query: &SlackChannelHistoryQuery) -> Result<(), ApiError> {
    if let Some(latest) = query.latest.as_deref() {
        validate_slack_timestamp(latest)?;
    }
    if let Some(oldest) = query.oldest.as_deref() {
        validate_slack_timestamp(oldest)?;
    }
    if let Some(limit) = query.limit
        && !(1..=999).contains(&limit)
    {
        return Err(ApiError::BadRequest(
            "Slack history limit must be between 1 and 999".to_owned(),
        ));
    }
    if let Some(cursor) = query.cursor.as_deref() {
        validate_slack_cursor(cursor)?;
    }
    Ok(())
}

fn validate_slack_channel_members_query(query: &SlackChannelMembersQuery) -> Result<(), ApiError> {
    if let Some(limit) = query.limit
        && !(1..=1000).contains(&limit)
    {
        return Err(ApiError::BadRequest(
            "Slack channel members limit must be between 1 and 1000".to_owned(),
        ));
    }
    if let Some(cursor) = query.cursor.as_deref() {
        validate_slack_cursor(cursor)?;
    }
    Ok(())
}

fn validate_slack_files_list_query(query: &SlackFilesListQuery) -> Result<(), ApiError> {
    if let Some(limit) = query.limit
        && !(1..=MAX_SLACK_FILES_LIST_LIMIT).contains(&limit)
    {
        return Err(ApiError::BadRequest(format!(
            "Slack files.list limit must be between 1 and {MAX_SLACK_FILES_LIST_LIMIT}"
        )));
    }
    let Some(channel_id) = query.channel_id.as_deref() else {
        return Err(ApiError::BadRequest(
            "Slack files.list channel_id is required".to_owned(),
        ));
    };
    validate_slack_channel_id(channel_id)?;
    if let Some(page) = query.page
        && page == 0
    {
        return Err(ApiError::BadRequest(
            "Slack files.list page must be greater than 0".to_owned(),
        ));
    }
    Ok(())
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

fn validate_slack_timestamp(timestamp: &str) -> Result<(), ApiError> {
    if !timestamp.is_empty()
        && timestamp
            .split_once('.')
            .map(|(seconds, micros)| {
                !seconds.is_empty()
                    && !micros.is_empty()
                    && seconds.bytes().all(|byte| byte.is_ascii_digit())
                    && micros.bytes().all(|byte| byte.is_ascii_digit())
            })
            .unwrap_or_else(|| timestamp.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Ok(());
    }
    Err(ApiError::BadRequest("invalid Slack timestamp".to_owned()))
}

fn validate_slack_cursor(cursor: &str) -> Result<(), ApiError> {
    if cursor.is_empty() || cursor.len() > 4096 || cursor.chars().any(|ch| ch.is_ascii_control()) {
        return Err(ApiError::BadRequest("invalid Slack cursor".to_owned()));
    }
    Ok(())
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
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

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

    fn test_channel(value: Value) -> SlackChannel {
        serde_json::from_value(value).unwrap()
    }

    fn test_channel_item(id: &str, name: &str, can_read_history: bool) -> SlackChannelItem {
        SlackChannelItem {
            id: id.to_owned(),
            name: name.to_owned(),
            purpose: String::new(),
            topic: String::new(),
            member_count: 0,
            is_private: false,
            is_member: true,
            can_upload: false,
            can_download: false,
            can_read_history,
        }
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
                    "history_channels": ["C111111111"]
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
        assert_eq!(claims.slack.upload_channels, ["C123456789"]);
        assert_eq!(claims.slack.download_channels, ["C987654321"]);
        assert_eq!(claims.slack.history_channels, ["C111111111"]);
    }

    #[test]
    fn extracts_deduped_channel_ids_from_all_slack_claims() {
        let claims = SlackFileProxyClaims {
            slack: SlackProxyClaims {
                upload_channels: vec!["C123456789".to_owned()],
                download_channels: vec!["G123456789".to_owned(), "C123456789".to_owned()],
                history_channels: vec!["D123456789".to_owned(), "G123456789".to_owned()],
            },
        };

        assert_eq!(
            slack_channel_ids_from_claims(&claims).unwrap(),
            vec![
                "C123456789".to_owned(),
                "D123456789".to_owned(),
                "G123456789".to_owned(),
            ]
        );
    }

    #[test]
    fn channel_item_enriches_slack_metadata_with_permissions() {
        let claims = SlackFileProxyClaims {
            slack: SlackProxyClaims {
                upload_channels: vec!["C123456789".to_owned()],
                download_channels: vec![],
                history_channels: vec!["C123456789".to_owned()],
            },
        };
        let channel = test_channel(json!({
            "id": "C123456789",
            "name": "general",
            "purpose": {"value": "Company updates"},
            "topic": {"value": "Announcements"},
            "num_members": 42,
            "is_private": false,
            "is_member": true
        }));

        let item = slack_channel_item(&claims, "C123456789", &channel);

        assert_eq!(item.id, "C123456789");
        assert_eq!(item.name, "general");
        assert_eq!(item.purpose, "Company updates");
        assert_eq!(item.topic, "Announcements");
        assert_eq!(item.member_count, 42);
        assert!(!item.is_private);
        assert!(item.is_member);
        assert!(item.can_upload);
        assert!(item.can_download);
        assert!(item.can_read_history);
    }

    #[test]
    fn slack_channel_requires_an_id() {
        assert!(
            serde_json::from_value::<SlackChannel>(json!({
                "is_private": false,
                "is_member": true
            }))
            .is_err()
        );
    }

    #[test]
    fn public_channel_defaults_require_bot_membership() {
        let accessible = test_channel(json!({
            "id": "C123456789",
            "is_private": false,
            "is_member": true
        }));
        assert!(slack_channel_has_default_access(&accessible));

        for inaccessible in [
            test_channel(json!({"id": "G123456789", "is_private": true, "is_member": true})),
            test_channel(json!({"id": "C123456789", "is_private": false, "is_member": false})),
        ] {
            assert!(!slack_channel_has_default_access(&inaccessible));
        }
    }

    #[tokio::test]
    async fn channel_authorization_uses_one_policy_for_all_permissions() {
        let requests = Arc::new(AtomicUsize::new(0));
        let handler_requests = requests.clone();
        let app = axum::Router::new().route(
            "/conversations.info",
            axum::routing::post(
                move |axum::extract::Form(form): axum::extract::Form<BTreeMap<String, String>>| {
                    let requests = handler_requests.clone();
                    async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        let channel_id = form.get("channel").unwrap();
                        axum::Json(json!({
                            "ok": true,
                            "channel": {
                                "id": channel_id,
                                "is_private": channel_id.starts_with('G'),
                                "is_member": true
                            }
                        }))
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = SlackFileProxyConfig {
            api_url: format!("http://{address}"),
            bot_token: "test-token".to_owned(),
            max_upload_bytes: 1,
        };
        let mut claims = SlackFileProxyClaims {
            slack: SlackProxyClaims {
                upload_channels: vec![],
                download_channels: vec![],
                history_channels: vec![],
            },
        };

        for permission in [
            SlackChannelPermission::Upload,
            SlackChannelPermission::Download,
            SlackChannelPermission::History,
        ] {
            ensure_slack_channel_allowed(&claims, http_client(), &config, "C123456789", permission)
                .await
                .unwrap();
        }

        let error = ensure_slack_channel_allowed(
            &claims,
            http_client(),
            &config,
            "G123456789",
            SlackChannelPermission::Download,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ApiError::Forbidden(_)));

        assert_eq!(requests.load(Ordering::SeqCst), 2);
        claims.slack.download_channels = vec!["G123456789".to_owned()];
        ensure_slack_channel_allowed(
            &claims,
            http_client(),
            &config,
            "G123456789",
            SlackChannelPermission::Download,
        )
        .await
        .unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn slack_api_retries_rate_limits() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let handler_attempts = attempts.clone();
        let app = axum::Router::new().route(
            "/conversations.info",
            axum::routing::post(move || {
                let attempts = handler_attempts.clone();
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (
                            axum::http::StatusCode::TOO_MANY_REQUESTS,
                            [(axum::http::header::RETRY_AFTER, "0")],
                            axum::Json(json!({"ok": false, "error": "ratelimited"})),
                        )
                            .into_response();
                    }
                    axum::Json(json!({"ok": true, "channel": {"id": "C123456789"}})).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let config = SlackFileProxyConfig {
            api_url: format!("http://{address}"),
            bot_token: "test-token".to_owned(),
            max_upload_bytes: 1,
        };

        let value = slack_api_post_form(
            http_client(),
            &config,
            "conversations.info",
            &[("channel", "C123456789".to_owned())],
        )
        .await
        .unwrap();

        assert_eq!(value.get("ok"), Some(&Value::Bool(true)));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[test]
    fn slack_rate_limit_delay_honors_retry_after_and_caps_it() {
        assert_eq!(
            slack_rate_limit_retry_delay(0, None),
            Duration::from_secs(1)
        );
        assert_eq!(
            slack_rate_limit_retry_delay(1, None),
            Duration::from_secs(2)
        );
        assert_eq!(
            slack_rate_limit_retry_delay(0, Some(30)),
            Duration::from_secs(30)
        );
        assert_eq!(
            slack_rate_limit_retry_delay(0, Some(600)),
            SLACK_RATE_LIMIT_MAX_DELAY
        );
    }

    #[test]
    fn channel_cache_entries_expire_after_five_minutes() {
        let fetched_at = Instant::now();
        let cached = CachedSlackValue::new(fetched_at, "channel");
        assert_eq!(cached.get_if_fresh(fetched_at), Some("channel"));
        assert_eq!(
            cached.get_if_fresh(fetched_at + SLACK_CHANNEL_CACHE_TTL),
            None
        );
    }

    #[test]
    fn public_channel_cache_serves_stale_data_while_refresh_is_deferred() {
        let fetched_at = Instant::now();
        let now = fetched_at + SLACK_CHANNEL_CACHE_TTL;
        let channel = test_channel(json!({
            "id": "C123456789",
            "is_private": false,
            "is_member": true
        }));
        let cache = SlackPublicChannelCacheState {
            channels: Some(CachedSlackValue::new(fetched_at, vec![channel])),
            retry_refresh_at: Some(now + SLACK_CHANNEL_REFRESH_RETRY_DELAY),
            refreshing: false,
        };

        assert_eq!(cache.stale_channels().unwrap()[0].id, "C123456789");
        assert!(!cache.can_refresh(now));
        assert!(cache.can_refresh(now + SLACK_CHANNEL_REFRESH_RETRY_DELAY));
    }

    #[tokio::test]
    async fn panicked_public_channel_refresh_clears_refreshing_state() {
        async fn panic_during_refresh() -> Result<Vec<SlackChannel>, ApiError> {
            panic!("simulated Slack channel refresh panic");
        }

        let cache = SlackPublicChannelCache::default();
        cache.lock().await.refreshing = true;
        let refresh = tokio::spawn(panic_during_refresh());

        assert!(
            await_slack_public_channel_refresh(refresh, &cache)
                .await
                .is_err()
        );
        let cache = cache.lock().await;
        assert!(!cache.refreshing);
        assert!(cache.retry_refresh_at.is_some());
    }

    #[test]
    fn proxy_channel_pagination_filters_before_slicing() {
        let query = SlackChannelsQuery {
            limit: Some(1),
            cursor: None,
            query: Some("a".to_owned()),
            history_only: true,
        };
        let (page, total_count, next_cursor) = paginate_slack_channels(
            vec![
                test_channel_item("C333333333", "Zulu", true),
                test_channel_item("G111111111", "Alpha private", true),
                test_channel_item("C222222222", "Beta", false),
            ],
            &query,
            1,
            0,
        );

        assert_eq!(page[0].id, "G111111111");
        assert_eq!(total_count, 1);
        assert_eq!(next_cursor, "");
    }

    #[test]
    fn proxy_channel_pagination_returns_a_next_cursor() {
        let query = SlackChannelsQuery {
            limit: Some(1),
            cursor: None,
            query: None,
            history_only: false,
        };
        let (page, total_count, next_cursor) = paginate_slack_channels(
            vec![
                test_channel_item("C222222222", "Beta", true),
                test_channel_item("C111111111", "Alpha", true),
            ],
            &query,
            1,
            0,
        );

        assert_eq!(page[0].id, "C111111111");
        assert_eq!(total_count, 2);
        assert_eq!(next_cursor, "1");
    }

    #[test]
    fn bot_channel_listing_requests_public_channels_only() {
        assert_eq!(
            slack_public_channels_form("cursor-1"),
            vec![
                ("types", "public_channel".to_owned()),
                ("exclude_archived", "true".to_owned()),
                ("limit", "200".to_owned()),
                ("cursor", "cursor-1".to_owned()),
            ]
        );
    }

    #[test]
    fn channel_info_form_requests_member_counts() {
        assert_eq!(
            slack_channel_info_form("C123456789"),
            vec![
                ("channel", "C123456789".to_owned()),
                ("include_num_members", "true".to_owned()),
            ]
        );
    }

    #[test]
    fn files_list_form_maps_proxy_query_to_slack_params() {
        let query = SlackFilesListQuery {
            channel_id: Some("C123456789".to_owned()),
            limit: Some(20),
            page: Some(3),
        };

        assert_eq!(
            slack_files_list_form("C123456789", &query),
            vec![
                ("channel", "C123456789".to_owned()),
                ("count", "20".to_owned()),
                ("page", "3".to_owned()),
            ]
        );
    }

    #[test]
    fn files_list_form_defaults_to_capped_first_page() {
        let query = SlackFilesListQuery {
            channel_id: Some("C123456789".to_owned()),
            limit: None,
            page: None,
        };

        assert_eq!(
            slack_files_list_form("C123456789", &query),
            vec![
                ("channel", "C123456789".to_owned()),
                ("count", DEFAULT_SLACK_FILES_LIST_LIMIT.to_string()),
                ("page", "1".to_owned()),
            ]
        );
    }

    #[test]
    fn channel_members_form_maps_proxy_query_to_slack_params() {
        let query = SlackChannelMembersQuery {
            limit: Some(500),
            cursor: Some("cursor-1".to_owned()),
        };

        assert_eq!(
            slack_channel_members_form("C123456789", &query),
            vec![
                ("channel", "C123456789".to_owned()),
                ("limit", "500".to_owned()),
                ("cursor", "cursor-1".to_owned()),
            ]
        );
    }

    #[test]
    fn file_info_form_maps_proxy_query_to_slack_params() {
        assert_eq!(
            slack_file_info_form("F123456789"),
            vec![("file", "F123456789".to_owned())]
        );
    }

    #[test]
    fn files_list_has_more_reads_legacy_paging() {
        assert!(slack_files_list_has_more(&json!({
            "paging": {"page": 1, "pages": 2}
        })));
        assert!(!slack_files_list_has_more(&json!({
            "paging": {"page": 2, "pages": 2}
        })));
    }

    #[test]
    fn validates_files_list_query() {
        validate_slack_files_list_query(&SlackFilesListQuery {
            channel_id: Some("C123456789".to_owned()),
            limit: Some(200),
            page: Some(1),
        })
        .unwrap();

        assert!(matches!(
            validate_slack_files_list_query(&SlackFilesListQuery {
                channel_id: None,
                limit: Some(20),
                page: None,
            })
            .unwrap_err(),
            ApiError::BadRequest(_)
        ));
        assert!(matches!(
            validate_slack_files_list_query(&SlackFilesListQuery {
                channel_id: Some("C123456789".to_owned()),
                limit: Some(201),
                page: None,
            })
            .unwrap_err(),
            ApiError::BadRequest(_)
        ));
        assert!(matches!(
            validate_slack_files_list_query(&SlackFilesListQuery {
                channel_id: Some("C123456789".to_owned()),
                limit: Some(20),
                page: Some(0),
            })
            .unwrap_err(),
            ApiError::BadRequest(_)
        ));
    }

    #[test]
    fn validates_channel_members_query() {
        validate_slack_channel_members_query(&SlackChannelMembersQuery {
            limit: Some(1000),
            cursor: Some("cursor-1".to_owned()),
        })
        .unwrap();

        assert!(matches!(
            validate_slack_channel_members_query(&SlackChannelMembersQuery {
                limit: Some(1001),
                cursor: None,
            })
            .unwrap_err(),
            ApiError::BadRequest(_)
        ));
        assert!(matches!(
            validate_slack_channel_members_query(&SlackChannelMembersQuery {
                limit: Some(10),
                cursor: Some("\n".to_owned()),
            })
            .unwrap_err(),
            ApiError::BadRequest(_)
        ));
    }

    #[tokio::test]
    async fn file_info_authorizes_before_reading_slack_config() {
        let headers = HeaderMap::new();
        let result =
            authorized_slack_file_info(&headers, http_client(), "F123456789", "C123456789").await;

        assert!(matches!(result, Err(ApiError::Unauthorized(_))));
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
        assert_eq!(claims.slack.upload_channels, ["C123456789"]);
        assert_eq!(claims.slack.download_channels, ["C123456789"]);
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

    #[test]
    fn validates_slack_channel_history_query() {
        validate_slack_channel_history_query(&SlackChannelHistoryQuery {
            latest: Some("1700000000.000002".to_owned()),
            oldest: Some("0".to_owned()),
            inclusive: Some(true),
            include_all_metadata: Some(true),
            limit: Some(999),
            cursor: Some("next_cursor".to_owned()),
        })
        .unwrap();

        assert!(matches!(
            validate_slack_channel_history_query(&SlackChannelHistoryQuery {
                latest: None,
                oldest: None,
                inclusive: None,
                include_all_metadata: None,
                limit: Some(1000),
                cursor: None,
            })
            .unwrap_err(),
            ApiError::BadRequest(_)
        ));
        assert!(matches!(
            validate_slack_channel_history_query(&SlackChannelHistoryQuery {
                latest: Some("not-a-ts".to_owned()),
                oldest: None,
                inclusive: None,
                include_all_metadata: None,
                limit: None,
                cursor: None,
            })
            .unwrap_err(),
            ApiError::BadRequest(_)
        ));
        assert!(matches!(
            validate_slack_channel_history_query(&SlackChannelHistoryQuery {
                latest: None,
                oldest: None,
                inclusive: None,
                include_all_metadata: None,
                limit: None,
                cursor: Some("bad\ncursor".to_owned()),
            })
            .unwrap_err(),
            ApiError::BadRequest(_)
        ));
    }

    #[test]
    fn channel_history_form_omits_empty_query_params() {
        let form = slack_channel_history_form(
            "C123456789",
            &SlackChannelHistoryQuery {
                latest: Some("1700000000.000002".to_owned()),
                oldest: None,
                inclusive: Some(false),
                include_all_metadata: Some(true),
                limit: Some(15),
                cursor: None,
            },
        );
        assert_eq!(
            form,
            vec![
                ("channel", "C123456789".to_owned()),
                ("latest", "1700000000.000002".to_owned()),
                ("inclusive", "false".to_owned()),
                ("include_all_metadata", "true".to_owned()),
                ("limit", "15".to_owned()),
            ]
        );
    }

    #[test]
    fn thread_replies_form_includes_thread_ts() {
        let form = slack_thread_replies_form(
            "C123456789",
            "1700000000.000001",
            &SlackChannelHistoryQuery {
                latest: None,
                oldest: Some("0".to_owned()),
                inclusive: Some(true),
                include_all_metadata: None,
                limit: Some(25),
                cursor: Some("next".to_owned()),
            },
        );
        assert_eq!(
            form,
            vec![
                ("channel", "C123456789".to_owned()),
                ("oldest", "0".to_owned()),
                ("inclusive", "true".to_owned()),
                ("limit", "25".to_owned()),
                ("cursor", "next".to_owned()),
                ("ts", "1700000000.000001".to_owned()),
            ]
        );
    }
}
