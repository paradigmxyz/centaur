//! `/broker_credentials` (bearer) and `/api/v1/console/broker_credentials`:
//! operator CRUD for managed broker credentials. Initial secret values are
//! write-only; the access token is never serialized.

use axum::{
    extract::{RawQuery, State},
    response::IntoResponse,
};
use centaur_console_db::{
    Ciphertext, Encryptor, decode_oid, oid,
    sync_config::{GRANT_COLUMNS, bump_sync_config, grantee_ids},
};
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use http::StatusCode;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::{PgConnection, Postgres, QueryBuilder};

use super::{URL_SAFE_MESSAGE, blank, error_response, label_filter, nullable, url_safe};
use crate::{
    ApiError, ApiResult, AppState, FieldErrors,
    auth::{BearerCaller, ConsoleCaller},
    extract::{Data, Path, Query, data},
    pagination::PageParams,
};

pub const GRANTS: [&str; 4] = ["refresh_token", "client_credentials", "password", "preqin"];
pub const PREQIN_TOKEN_ENDPOINT: &str = "https://api.preqin.com/connect/token";
pub const DEFAULT_SLACK_SECONDS: i32 = 300;
pub const DEFAULT_FRACTION: f64 = 0.2;
pub const DEFAULT_MAX_INTERVAL_SECONDS: i32 = 86_400;
pub const DEFAULT_REFRESH_TIMEOUT_SECONDS: i32 = 30;
pub const REFERENCED_MESSAGE: &str =
    "is referenced by one or more token_broker secret sources; remove those references first";
const REFRESH_FLOOR_SECONDS: f64 = 60.0;

// --- reading ----------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    foreign_id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    labels: Value,
    grant: String,
    token_endpoint: String,
    scopes: Value,
    client_id: Option<String>,
    token_endpoint_headers: Option<Ciphertext>,
    early_refresh_slack_seconds: i32,
    early_refresh_fraction: f64,
    max_refresh_interval_seconds: i32,
    refresh_timeout_seconds: i32,
    expires_at: Option<DateTime<Utc>>,
    last_refresh: Option<DateTime<Utc>>,
    next_attempt_at: Option<DateTime<Utc>>,
    dead: bool,
    dead_reason: Option<String>,
    failure_count: i32,
    provider_subject: Option<String>,
    provider_email: Option<String>,
    external_user_key: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    has_client_secret: bool,
    has_refresh_token: bool,
    has_username: bool,
    has_password: bool,
    has_api_key: bool,
    app_id: Option<i64>,
    app_slug: Option<String>,
    app_provider: Option<String>,
    app_client_id: Option<String>,
    secret_id: Option<i64>,
    secret_foreign_id: Option<String>,
    secret_name: Option<String>,
}

const SELECT: &str = r#"SELECT bc.id, bc.foreign_id, bc.name, bc.description, bc.labels,
    bc."grant" AS "grant", bc.token_endpoint, bc.scopes, bc.client_id, bc.token_endpoint_headers,
    bc.early_refresh_slack_seconds, bc.early_refresh_fraction, bc.max_refresh_interval_seconds,
    bc.refresh_timeout_seconds,
    bc.expires_at AT TIME ZONE 'UTC' AS expires_at,
    bc.last_refresh AT TIME ZONE 'UTC' AS last_refresh,
    bc.next_attempt_at AT TIME ZONE 'UTC' AS next_attempt_at,
    bc.dead, bc.dead_reason, bc.failure_count, bc.provider_subject, bc.provider_email,
    bc.external_user_key,
    bc.created_at AT TIME ZONE 'UTC' AS created_at, bc.updated_at AT TIME ZONE 'UTC' AS updated_at,
    (bc.client_secret IS NOT NULL) AS has_client_secret,
    (bc.refresh_token IS NOT NULL) AS has_refresh_token,
    (bc.username IS NOT NULL) AS has_username,
    (bc.password IS NOT NULL) AS has_password,
    (bc.api_key IS NOT NULL) AS has_api_key,
    oa.id AS app_id, oa.slug AS app_slug, oa.provider AS app_provider,
    oa.client_id AS app_client_id,
    ss.id AS secret_id, ss.foreign_id AS secret_foreign_id, ss.name AS secret_name
  FROM broker_credentials bc
  LEFT JOIN oauth_apps oa ON oa.id = bc.oauth_app_id
  LEFT JOIN LATERAL (SELECT id, foreign_id, name FROM static_secrets
                     WHERE broker_credential_id = bc.id ORDER BY id LIMIT 1) ss ON TRUE"#;

pub(crate) fn status(dead: bool, last_refresh: Option<DateTime<Utc>>) -> &'static str {
    if dead {
        "dead"
    } else if last_refresh.is_none() {
        "bootstrapping"
    } else {
        "live"
    }
}

/// The JSON of a credential. `console` adds presence flags for the
/// write-only values and the linked app and secret.
fn payload(row: &Row, encryptor: &Encryptor, console: bool) -> Result<Value, ApiError> {
    let header_names: Vec<String> = match &row.token_endpoint_headers {
        Some(cipher) => serde_json::from_str::<Map<String, Value>>(&encryptor.decrypt(cipher)?)
            .map(|headers| headers.keys().cloned().collect())
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let mut payload = json!({
        "id": oid("bcr", row.id),
        "foreign_id": row.foreign_id,
        "name": row.name,
        "description": row.description,
        "labels": row.labels,
        "grant": row.grant,
        "token_endpoint": row.token_endpoint,
        "scopes": row.scopes,
        "client_id": row.client_id,
        "token_endpoint_header_names": header_names,
        "early_refresh_slack_seconds": row.early_refresh_slack_seconds,
        "early_refresh_fraction": row.early_refresh_fraction,
        "max_refresh_interval_seconds": row.max_refresh_interval_seconds,
        "refresh_timeout_seconds": row.refresh_timeout_seconds,
        "status": status(row.dead, row.last_refresh),
        "expires_at": row.expires_at,
        "last_refresh": row.last_refresh,
        "next_attempt_at": row.next_attempt_at,
        "dead": row.dead,
        "dead_reason": row.dead_reason,
        "failure_count": row.failure_count,
        "oauth_app_id": row.app_id.map(|id| oid("oap", id)),
        "provider_subject": row.provider_subject,
        "provider_email": row.provider_email,
        "external_user_key": row.external_user_key,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    });
    if console {
        let extra = json!({
            "has_client_secret": row.has_client_secret,
            "has_refresh_token": row.has_refresh_token,
            "has_username": row.has_username,
            "has_password": row.has_password,
            "has_api_key": row.has_api_key,
            "effective_client_id": row.app_client_id.clone().or_else(|| row.client_id.clone()),
            "oauth_app": row.app_id.map(|id| json!({
                "id": oid("oap", id), "slug": row.app_slug, "provider": row.app_provider,
            })),
            "static_secret": row.secret_id.map(|id| json!({
                "id": oid("ssr", id), "foreign_id": row.secret_foreign_id, "name": row.secret_name,
            })),
        });
        if let (Some(payload), Value::Object(extra)) = (payload.as_object_mut(), extra) {
            payload.extend(extra);
        }
    }
    Ok(payload)
}

async fn find(state: &AppState, id: i64) -> Result<Row, ApiError> {
    sqlx::query_as(&format!("{SELECT} WHERE bc.id = $1"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(ApiError::not_found)
}

fn credential_id(id: &str) -> Result<i64, ApiError> {
    decode_oid("bcr", id).ok_or_else(ApiError::not_found)
}

async fn index(state: &AppState, raw: &RawQuery, page: PageParams, console: bool) -> ApiResult {
    let labels = label_filter(raw);
    let filter = (!labels.is_empty()).then_some(Value::Object(labels));
    let page = page.resolve();
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM broker_credentials WHERE ($1::jsonb IS NULL OR labels @> $1)",
    )
    .bind(&filter)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} WHERE ($1::jsonb IS NULL OR bc.labels @> $1) \
         ORDER BY bc.created_at, bc.id LIMIT $2 OFFSET $3"
    ))
    .bind(&filter)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    let rows = rows
        .iter()
        .map(|row| payload(row, &state.encryptor, console))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(axum::Json(json!({"data": rows, "meta": page.meta(total)})).into_response())
}

pub async fn bearer_index(
    State(state): State<AppState>,
    _caller: BearerCaller,
    raw: RawQuery,
    Query(page): Query<PageParams>,
) -> ApiResult {
    index(&state, &raw, page, false).await
}

pub async fn console_index(
    State(state): State<AppState>,
    _caller: ConsoleCaller,
    raw: RawQuery,
    Query(page): Query<PageParams>,
) -> ApiResult {
    index(&state, &raw, page, true).await
}

async fn show(state: &AppState, id: &str, console: bool) -> ApiResult {
    let row = find(state, credential_id(id)?).await?;
    Ok(data(payload(&row, &state.encryptor, console)?).into_response())
}

pub async fn bearer_show(
    State(state): State<AppState>,
    _caller: BearerCaller,
    Path(id): Path<String>,
) -> ApiResult {
    show(&state, &id, false).await
}

pub async fn console_show(
    State(state): State<AppState>,
    _caller: ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    show(&state, &id, true).await
}

/// `GET /broker_credentials/lookup[/default]/{foreign_id}`.
pub async fn bearer_lookup(
    State(state): State<AppState>,
    _caller: BearerCaller,
    Path(foreign_id): Path<String>,
) -> ApiResult {
    let id: Option<i64> =
        sqlx::query_scalar("SELECT id FROM broker_credentials WHERE foreign_id = $1")
            .bind(&foreign_id)
            .fetch_optional(&state.db)
            .await?;
    let row = find(&state, id.ok_or_else(ApiError::not_found)?).await?;
    Ok(data(payload(&row, &state.encryptor, false)?).into_response())
}

// --- input ------------------------------------------------------------------------

/// A number as JSON sends it: an integer, a float, or text (form inputs).
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Num {
    Int(i64),
    Float(f64),
    Text(String),
}

impl Num {
    fn float(&self) -> Option<f64> {
        let value = match self {
            Self::Int(value) => *value as f64,
            Self::Float(value) => *value,
            Self::Text(text) => text.trim().parse().ok()?,
        };
        value.is_finite().then_some(value)
    }

    /// `Err(true)`: a number, but not an integer; `Err(false)`: not a number.
    fn integer(&self) -> Result<i64, bool> {
        match self {
            Self::Int(value) => Ok(*value),
            Self::Text(text) if text.trim().parse::<i64>().is_ok() => {
                Ok(text.trim().parse().unwrap_or_default())
            }
            other => match other.float() {
                Some(value) if value.fract() == 0.0 && value.abs() < 9e15 => Ok(value as i64),
                Some(_) => Err(true),
                None => Err(false),
            },
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct Input {
    #[serde(deserialize_with = "nullable")]
    foreign_id: Option<Option<String>>,
    #[serde(deserialize_with = "nullable")]
    name: Option<Option<String>>,
    #[serde(deserialize_with = "nullable")]
    description: Option<Option<String>>,
    token_endpoint: Option<String>,
    grant: Option<String>,
    #[serde(deserialize_with = "nullable")]
    client_id: Option<Option<String>>,
    early_refresh_slack_seconds: Option<Num>,
    early_refresh_fraction: Option<Num>,
    max_refresh_interval_seconds: Option<Num>,
    refresh_timeout_seconds: Option<Num>,
    labels: Option<Map<String, Value>>,
    scopes: Option<Vec<String>>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
    username: Option<String>,
    password: Option<String>,
    api_key: Option<String>,
    /// Header names to values; `null` clears them.
    #[serde(deserialize_with = "nullable")]
    token_endpoint_headers: Option<Option<Map<String, Value>>>,
}

impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Input")
            .field("foreign_id", &self.foreign_id)
            .field("name", &self.name)
            .field("grant", &self.grant)
            .finish_non_exhaustive()
    }
}

// --- the credential as it will be stored -------------------------------------------

/// A credential's writable state: loaded from the row (or the defaults),
/// changed by the request, validated, then diffed against the row so only
/// changed columns are written.
#[derive(Clone)]
struct Fields {
    foreign_id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    token_endpoint: String,
    grant: String,
    client_id: Option<String>,
    slack_seconds: i32,
    fraction: f64,
    max_interval_seconds: i32,
    timeout_seconds: i32,
    labels: Map<String, Value>,
    scopes: Vec<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
    username: Option<String>,
    password: Option<String>,
    api_key: Option<String>,
    headers: Option<Map<String, Value>>,
    dead: bool,
    dead_reason: Option<String>,
    failure_count: i32,
    next_attempt_at: Option<NaiveDateTime>,
}

impl std::fmt::Debug for Fields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fields")
            .field("foreign_id", &self.foreign_id)
            .field("grant", &self.grant)
            .finish_non_exhaustive()
    }
}

/// The columns [`Fields::load`] reads.
#[derive(sqlx::FromRow)]
struct Stored {
    id: i64,
    oauth_app_id: Option<i64>,
    foreign_id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    token_endpoint: String,
    grant: String,
    client_id: Option<String>,
    early_refresh_slack_seconds: i32,
    early_refresh_fraction: f64,
    max_refresh_interval_seconds: i32,
    refresh_timeout_seconds: i32,
    labels: Value,
    scopes: Value,
    client_secret: Option<Ciphertext>,
    refresh_token: Option<Ciphertext>,
    username: Option<Ciphertext>,
    password: Option<Ciphertext>,
    api_key: Option<Ciphertext>,
    token_endpoint_headers: Option<Ciphertext>,
    dead: bool,
    dead_reason: Option<String>,
    failure_count: i32,
    next_attempt_at: Option<NaiveDateTime>,
}

const SELECT_STORED: &str = r#"SELECT id, oauth_app_id, foreign_id, name, description, token_endpoint,
    "grant" AS "grant", client_id, early_refresh_slack_seconds, early_refresh_fraction,
    max_refresh_interval_seconds, refresh_timeout_seconds, labels, scopes, client_secret,
    refresh_token, username, password, api_key, token_endpoint_headers, dead, dead_reason,
    failure_count, next_attempt_at
  FROM broker_credentials"#;

impl Fields {
    fn new() -> Self {
        Self {
            foreign_id: None,
            name: None,
            description: None,
            token_endpoint: String::new(),
            grant: "refresh_token".into(),
            client_id: None,
            slack_seconds: DEFAULT_SLACK_SECONDS,
            fraction: DEFAULT_FRACTION,
            max_interval_seconds: DEFAULT_MAX_INTERVAL_SECONDS,
            timeout_seconds: DEFAULT_REFRESH_TIMEOUT_SECONDS,
            labels: Map::new(),
            scopes: Vec::new(),
            client_secret: None,
            refresh_token: None,
            username: None,
            password: None,
            api_key: None,
            headers: None,
            dead: false,
            dead_reason: None,
            failure_count: 0,
            next_attempt_at: None,
        }
    }

    fn load(row: &Stored, encryptor: &Encryptor) -> Result<Self, ApiError> {
        let decrypt = |cipher: &Option<Ciphertext>| -> Result<Option<String>, ApiError> {
            Ok(encryptor.decrypt_optional(cipher.as_ref())?)
        };
        Ok(Self {
            foreign_id: row.foreign_id.clone(),
            name: row.name.clone(),
            description: row.description.clone(),
            token_endpoint: row.token_endpoint.clone(),
            grant: row.grant.clone(),
            client_id: row.client_id.clone(),
            slack_seconds: row.early_refresh_slack_seconds,
            fraction: row.early_refresh_fraction,
            max_interval_seconds: row.max_refresh_interval_seconds,
            timeout_seconds: row.refresh_timeout_seconds,
            labels: row.labels.as_object().cloned().unwrap_or_default(),
            scopes: serde_json::from_value(row.scopes.clone()).unwrap_or_default(),
            client_secret: decrypt(&row.client_secret)?,
            refresh_token: decrypt(&row.refresh_token)?,
            username: decrypt(&row.username)?,
            password: decrypt(&row.password)?,
            api_key: decrypt(&row.api_key)?,
            headers: decrypt(&row.token_endpoint_headers)?
                .and_then(|text| serde_json::from_str(&text).ok()),
            dead: row.dead,
            dead_reason: row.dead_reason.clone(),
            failure_count: row.failure_count,
            next_attempt_at: row.next_attempt_at,
        })
    }

    /// Applies a request. `console` lets a `null` header value keep the
    /// stored value of that header (the SPA never sees header values).
    fn assign(
        &mut self,
        input: Input,
        now: NaiveDateTime,
        console: bool,
        errors: &mut FieldErrors,
    ) {
        // A blank or null foreign id never clears one that is set.
        if let Some(Some(foreign_id)) = input.foreign_id
            && !foreign_id.trim().is_empty()
        {
            self.foreign_id = Some(foreign_id);
        }
        if let Some(name) = input.name {
            self.name = name;
        }
        if let Some(description) = input.description {
            self.description = description;
        }
        if let Some(token_endpoint) = input.token_endpoint {
            self.token_endpoint = token_endpoint;
        }
        if let Some(grant) = input.grant {
            self.grant = grant;
        }
        if let Some(client_id) = input.client_id {
            self.client_id = client_id;
        }
        let integer = |name: &str, value: Option<Num>, errors: &mut FieldErrors| -> Option<i32> {
            let value = value?;
            match value.integer() {
                Ok(number) if number <= 0 => {
                    errors.add(name, "must be greater than 0");
                    None
                }
                Ok(number) => i32::try_from(number)
                    .inspect_err(|_| errors.add(name, "is too large"))
                    .ok(),
                Err(true) => {
                    errors.add(name, "must be an integer");
                    None
                }
                Err(false) => {
                    errors.add(name, "is not a number");
                    None
                }
            }
        };
        if let Some(value) = integer(
            "early_refresh_slack_seconds",
            input.early_refresh_slack_seconds,
            errors,
        ) {
            self.slack_seconds = value;
        }
        if let Some(value) = integer(
            "max_refresh_interval_seconds",
            input.max_refresh_interval_seconds,
            errors,
        ) {
            self.max_interval_seconds = value;
        }
        if let Some(value) = integer(
            "refresh_timeout_seconds",
            input.refresh_timeout_seconds,
            errors,
        ) {
            self.timeout_seconds = value;
        }
        if let Some(fraction) = input.early_refresh_fraction {
            match fraction.float() {
                None => errors.add("early_refresh_fraction", "is not a number"),
                Some(value) if value < 0.0 => errors.add(
                    "early_refresh_fraction",
                    "must be greater than or equal to 0",
                ),
                Some(value) if value >= 1.0 => {
                    errors.add("early_refresh_fraction", "must be less than 1")
                }
                Some(value) => self.fraction = value,
            }
        }
        if let Some(labels) = input.labels {
            self.labels = labels;
        }
        if let Some(scopes) = input.scopes {
            self.scopes = scopes;
        }
        if let Some(headers) = input.token_endpoint_headers {
            self.headers = match headers {
                None => None,
                Some(mut new) => {
                    if console && let Some(stored) = &self.headers {
                        for (name, value) in new.iter_mut() {
                            if value.is_null()
                                && let Some(kept) = stored.get(name)
                            {
                                *value = kept.clone();
                            }
                        }
                    }
                    Some(new)
                }
            };
        }
        // Write-only secrets: blank means "keep the stored value". Any new
        // credential material revives the credential and makes it due now.
        let present = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
        let mut revive = false;
        if let Some(secret) = present(input.client_secret) {
            self.client_secret = Some(secret);
            revive |= self.grant == "client_credentials";
        }
        for (slot, value) in [
            (&mut self.refresh_token, input.refresh_token),
            (&mut self.username, input.username),
            (&mut self.password, input.password),
            (&mut self.api_key, input.api_key),
        ] {
            if let Some(value) = present(value) {
                *slot = Some(value);
                revive = true;
            }
        }
        if revive {
            self.dead = false;
            self.dead_reason = None;
            self.failure_count = 0;
            self.next_attempt_at = Some(now);
        }
        if self.grant == "preqin" {
            self.token_endpoint = PREQIN_TOKEN_ENDPOINT.into();
        }
    }

    /// `app_secret`: the linked app's client secret, when there is one.
    fn validate(
        &self,
        linked_app: bool,
        app_secret: Option<&str>,
        stored_foreign_id: Option<&str>,
        errors: &mut FieldErrors,
    ) {
        if let Some(foreign_id) = self.foreign_id.as_deref() {
            if foreign_id.starts_with("bcr_") {
                errors.add(
                    "foreign_id",
                    "must not start with \"bcr_\", which is reserved for opaque ids",
                );
            }
            if !url_safe(foreign_id) {
                errors.add("foreign_id", URL_SAFE_MESSAGE);
            }
        }
        if stored_foreign_id.is_some_and(|stored| !stored.trim().is_empty())
            && stored_foreign_id != self.foreign_id.as_deref()
        {
            errors.add("foreign_id", "cannot be changed once set");
        }
        if !GRANTS.contains(&self.grant.as_str()) {
            errors.add(
                "grant",
                "must be one of refresh_token, client_credentials, password, preqin",
            );
        }
        if self.token_endpoint.trim().is_empty() {
            errors.add("token_endpoint", "can't be blank");
        }
        if self.grant != "preqin" && !linked_app && blank(self.client_id.as_deref()) {
            errors.add("client_id", "can't be blank");
        }
        let secret = if linked_app {
            app_secret
        } else {
            self.client_secret.as_deref()
        };
        match self.grant.as_str() {
            "client_credentials" if blank(secret) => {
                errors.add(
                    "client_secret",
                    "can't be blank for the client_credentials grant",
                );
            }
            "password" => {
                if blank(self.username.as_deref()) {
                    errors.add("username", "can't be blank for the password grant");
                }
                if blank(self.password.as_deref()) {
                    errors.add("password", "can't be blank for the password grant");
                }
            }
            "preqin" => {
                if blank(self.username.as_deref()) {
                    errors.add("username", "can't be blank for the Preqin broker grant");
                }
                if blank(self.api_key.as_deref()) {
                    errors.add("api_key", "can't be blank for the Preqin broker grant");
                }
            }
            _ => {}
        }
        if let Some(headers) = &self.headers
            && !headers.values().all(Value::is_string)
        {
            errors.add(
                "token_endpoint_headers",
                "must be an object mapping header names to string values",
            );
        }
    }

    fn headers_cipher(&self, encryptor: &Encryptor) -> Result<Option<Ciphertext>, ApiError> {
        match &self.headers {
            Some(headers) => Ok(Some(
                encryptor.encrypt(&serde_json::to_string(headers).map_err(ApiError::internal)?)?,
            )),
            None => Ok(None),
        }
    }
}

// --- writing ----------------------------------------------------------------------

/// Inserts a credential and returns its id.
async fn insert(
    conn: &mut PgConnection,
    fields: &Fields,
    created_by: i64,
    encryptor: &Encryptor,
    now: NaiveDateTime,
) -> Result<i64, ApiError> {
    let secret = |value: &Option<String>| encryptor.encrypt_optional(value.as_deref());
    sqlx::query_scalar(
        r#"INSERT INTO broker_credentials (foreign_id, name, description, token_endpoint, "grant",
             client_id, early_refresh_slack_seconds, early_refresh_fraction,
             max_refresh_interval_seconds, refresh_timeout_seconds, labels, scopes, client_secret,
             refresh_token, username, password, api_key, token_endpoint_headers, dead, dead_reason,
             failure_count, next_attempt_at, created_by_id, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
             $19, $20, $21, $22, $23, $24, $24) RETURNING id"#,
    )
    .bind(&fields.foreign_id)
    .bind(&fields.name)
    .bind(&fields.description)
    .bind(&fields.token_endpoint)
    .bind(&fields.grant)
    .bind(&fields.client_id)
    .bind(fields.slack_seconds)
    .bind(fields.fraction)
    .bind(fields.max_interval_seconds)
    .bind(fields.timeout_seconds)
    .bind(Value::Object(fields.labels.clone()))
    .bind(json!(fields.scopes))
    .bind(secret(&fields.client_secret)?)
    .bind(secret(&fields.refresh_token)?)
    .bind(secret(&fields.username)?)
    .bind(secret(&fields.password)?)
    .bind(secret(&fields.api_key)?)
    .bind(fields.headers_cipher(encryptor)?)
    .bind(fields.dead)
    .bind(&fields.dead_reason)
    .bind(fields.failure_count)
    .bind(fields.next_attempt_at)
    .bind(created_by)
    .bind(now)
    .fetch_one(conn)
    .await
    .map_err(|error| ApiError::unique(error, "foreign_id", "has already been taken"))
}

/// Updates only the columns whose value differs between `old` and `new`.
/// Returns whether anything was written.
async fn update(
    conn: &mut PgConnection,
    id: i64,
    old: &Fields,
    new: &Fields,
    encryptor: &Encryptor,
    now: NaiveDateTime,
) -> Result<bool, ApiError> {
    let mut query = QueryBuilder::<Postgres>::new("UPDATE broker_credentials SET ");
    let mut sets = query.separated(", ");
    let mut changed = false;
    macro_rules! set {
        ($column:literal, $field:ident) => {
            if old.$field != new.$field {
                changed = true;
                sets.push(concat!($column, " = "))
                    .push_bind_unseparated(new.$field.clone());
            }
        };
    }
    macro_rules! set_secret {
        ($column:literal, $field:ident) => {
            if old.$field != new.$field {
                changed = true;
                sets.push(concat!($column, " = "))
                    .push_bind_unseparated(encryptor.encrypt_optional(new.$field.as_deref())?);
            }
        };
    }
    set!("foreign_id", foreign_id);
    set!("name", name);
    set!("description", description);
    set!("token_endpoint", token_endpoint);
    set!("\"grant\"", grant);
    set!("client_id", client_id);
    set!("early_refresh_slack_seconds", slack_seconds);
    set!("early_refresh_fraction", fraction);
    set!("max_refresh_interval_seconds", max_interval_seconds);
    set!("refresh_timeout_seconds", timeout_seconds);
    set!("dead", dead);
    set!("dead_reason", dead_reason);
    set!("failure_count", failure_count);
    set!("next_attempt_at", next_attempt_at);
    set_secret!("client_secret", client_secret);
    set_secret!("refresh_token", refresh_token);
    set_secret!("username", username);
    set_secret!("password", password);
    set_secret!("api_key", api_key);
    if old.labels != new.labels {
        changed = true;
        sets.push("labels = ")
            .push_bind_unseparated(Value::Object(new.labels.clone()));
    }
    if old.scopes != new.scopes {
        changed = true;
        sets.push("scopes = ")
            .push_bind_unseparated(json!(new.scopes));
    }
    if old.headers != new.headers {
        changed = true;
        sets.push("token_endpoint_headers = ")
            .push_bind_unseparated(new.headers_cipher(encryptor)?);
    }
    if !changed {
        return Ok(false);
    }
    sets.push("updated_at = ").push_bind_unseparated(now);
    query.push(" WHERE id = ").push_bind(id);
    query
        .build()
        .execute(conn)
        .await
        .map_err(|error| ApiError::unique(error, "foreign_id", "has already been taken"))?;
    Ok(true)
}

/// Invalidates the sync config of every principal granted a secret that
/// serves `credential_id` through a token_broker source.
pub(crate) async fn bump_grantees(
    conn: &mut PgConnection,
    credential_id: i64,
) -> Result<(), ApiError> {
    let mut principals = Vec::new();
    for column in GRANT_COLUMNS {
        let owners: Vec<i64> = sqlx::query_scalar(&format!(
            "SELECT {column} FROM secret_sources \
             WHERE source_type = 'token_broker' AND broker_credential_id = $1 AND {column} IS NOT NULL"
        ))
        .bind(credential_id)
        .fetch_all(&mut *conn)
        .await?;
        for owner in owners {
            principals.extend(grantee_ids(conn, column, owner).await?);
        }
    }
    bump_sync_config(conn, &principals).await?;
    Ok(())
}

async fn app_secret(state: &AppState, app_id: Option<i64>) -> Result<Option<String>, ApiError> {
    let Some(app_id) = app_id else {
        return Ok(None);
    };
    let cipher: Option<Option<Ciphertext>> =
        sqlx::query_scalar("SELECT client_secret FROM oauth_apps WHERE id = $1")
            .bind(app_id)
            .fetch_optional(&state.db)
            .await?;
    Ok(state
        .encryptor
        .decrypt_optional(cipher.flatten().as_ref())?)
}

/// Creates (`existing: None`) or updates a credential, then returns its id
/// and whether it was created.
async fn save(
    state: &AppState,
    caller_id: i64,
    existing: Option<Stored>,
    input: Input,
    console: bool,
) -> Result<(i64, bool), ApiError> {
    let now = state.now().naive_utc();
    let mut errors = FieldErrors::new();
    let old = match &existing {
        Some(row) => Fields::load(row, &state.encryptor)?,
        None => Fields::new(),
    };
    let mut fields = old.clone();
    fields.assign(input, now, console, &mut errors);
    let linked_app = existing.as_ref().and_then(|row| row.oauth_app_id);
    let secret = app_secret(state, linked_app).await?;
    fields.validate(
        linked_app.is_some(),
        secret.as_deref(),
        old.foreign_id.as_deref(),
        &mut errors,
    );
    errors.finish()?;

    let mut tx = state.db.begin().await?;
    let (id, created) = match &existing {
        None => (
            insert(&mut tx, &fields, caller_id, &state.encryptor, now).await?,
            true,
        ),
        Some(row) => {
            update(&mut tx, row.id, &old, &fields, &state.encryptor, now).await?;
            (row.id, false)
        }
    };
    // Reviving a dead credential changes what proxy-sync serves.
    if old.dead && !fields.dead {
        bump_grantees(&mut tx, id).await?;
    }
    tx.commit().await?;
    if linked_app.is_some() {
        crate::principals::effects::credential_changed(state, id).await;
    }
    Ok((id, created))
}

pub async fn bearer_create(
    State(state): State<AppState>,
    BearerCaller(auth): BearerCaller,
    Data(input): Data<Input>,
) -> ApiResult {
    created(&state, auth.user.id, input, false).await
}

pub async fn console_create(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Data(input): Data<Input>,
) -> ApiResult {
    created(&state, user.id, input, true).await
}

async fn created(state: &AppState, caller_id: i64, input: Input, console: bool) -> ApiResult {
    let (id, _) = save(state, caller_id, None, input, console).await?;
    let row = find(state, id).await?;
    Ok((
        StatusCode::CREATED,
        data(payload(&row, &state.encryptor, console)?),
    )
        .into_response())
}

/// `PATCH/PUT /broker_credentials/{id}`: `id` is an opaque id, or a foreign
/// id (created when it does not exist yet).
async fn upsert(
    state: &AppState,
    caller_id: i64,
    id: &str,
    input: Input,
    console: bool,
) -> ApiResult {
    let existing: Option<Stored> = if id.starts_with("bcr_") {
        let row_id = credential_id(id)?;
        Some(
            sqlx::query_as(&format!("{SELECT_STORED} WHERE id = $1"))
                .bind(row_id)
                .fetch_optional(&state.db)
                .await?
                .ok_or_else(ApiError::not_found)?,
        )
    } else {
        sqlx::query_as(&format!("{SELECT_STORED} WHERE foreign_id = $1"))
            .bind(id)
            .fetch_optional(&state.db)
            .await?
    };
    let mut input = input;
    if existing.is_none() {
        input.foreign_id = Some(Some(id.to_owned()));
    }
    let (row_id, created) = save(state, caller_id, existing, input, console).await?;
    let row = find(state, row_id).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, data(payload(&row, &state.encryptor, console)?)).into_response())
}

pub async fn bearer_update(
    State(state): State<AppState>,
    BearerCaller(auth): BearerCaller,
    Path(id): Path<String>,
    Data(input): Data<Input>,
) -> ApiResult {
    upsert(&state, auth.user.id, &id, input, false).await
}

pub async fn console_update(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Path(id): Path<String>,
    Data(input): Data<Input>,
) -> ApiResult {
    upsert(&state, user.id, &id, input, true).await
}

/// `DELETE`: unlinks the wrapping secret, refused (409) while a token_broker
/// source references the credential.
pub async fn destroy(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = credential_id(&id)?;
    let mut tx = state.db.begin().await?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM broker_credentials WHERE id = $1)")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if !exists {
        return Err(ApiError::not_found());
    }
    let referenced: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM secret_sources \
         WHERE source_type = 'token_broker' AND broker_credential_id = $1)",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if referenced {
        return Ok(error_response(StatusCode::CONFLICT, REFERENCED_MESSAGE));
    }
    sqlx::query(
        "UPDATE static_secrets SET broker_credential_id = NULL WHERE broker_credential_id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM broker_credentials WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// When a refresh is due: early by the larger of the fixed slack and the
/// fraction of the token's lifetime (at least a minute), but never later than
/// the maximum refresh interval after `last_refresh`.
pub(crate) fn next_attempt_at(
    now: NaiveDateTime,
    expires_at: Option<NaiveDateTime>,
    last_refresh: NaiveDateTime,
    slack_seconds: i32,
    fraction: f64,
    max_interval_seconds: i32,
) -> NaiveDateTime {
    let Some(expires_at) = expires_at else {
        return now;
    };
    let lifetime = (expires_at - last_refresh).num_microseconds().unwrap_or(0) as f64 / 1e6;
    let mut slack = f64::from(slack_seconds);
    if fraction > 0.0 && lifetime > 0.0 && lifetime * fraction > slack {
        slack = lifetime * fraction;
    }
    let slack = slack.max(REFRESH_FLOOR_SECONDS);
    let early = expires_at - Duration::microseconds((slack * 1e6).round() as i64);
    early.min(last_refresh + Duration::seconds(i64::from(max_interval_seconds)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> NaiveDateTime {
        DateTime::from_timestamp(1_800_000_000 + seconds, 0)
            .unwrap()
            .naive_utc()
    }

    #[test]
    fn next_attempt_uses_the_larger_slack_with_a_floor_and_ceiling() {
        let now = at(0);
        assert_eq!(next_attempt_at(now, None, now, 300, 0.2, 86_400), now);
        // 20% of an hour beats the 5 minute slack.
        assert_eq!(
            next_attempt_at(now, Some(at(3600)), now, 300, 0.2, 86_400),
            at(3600 - 720)
        );
        // A 10 second slack is raised to the one minute floor; the max
        // interval caps the result.
        assert_eq!(
            next_attempt_at(now, Some(at(1000)), now, 10, 0.0, 86_400),
            at(940)
        );
        assert_eq!(
            next_attempt_at(now, Some(at(100_000)), now, 10, 0.0, 100),
            at(100)
        );
    }

    #[test]
    fn numbers_may_arrive_as_text() {
        assert_eq!(Num::Text("3600".into()).integer(), Ok(3600));
        assert_eq!(Num::Float(2.0).integer(), Ok(2));
        assert_eq!(Num::Text("1.5".into()).integer(), Err(true));
        assert_eq!(Num::Text("x".into()).integer(), Err(false));
        assert_eq!(Num::Text("0.25".into()).float(), Some(0.25));
    }

    #[test]
    fn validation_follows_the_grant() {
        let check = |fields: &Fields, linked: bool, secret: Option<&str>, stored: Option<&str>| {
            let mut errors = FieldErrors::new();
            fields.validate(linked, secret, stored, &mut errors);
            errors.is_empty()
        };
        let mut fields = Fields::new();
        fields.token_endpoint = "https://idp.example/token".into();
        fields.grant = "client_credentials".into();
        fields.client_id = Some("cid".into());
        assert!(!check(&fields, false, None, None));
        fields.client_secret = Some("s".into());
        assert!(check(&fields, false, None, None));
        // A linked app supplies its own secret and client id.
        fields.client_secret = None;
        fields.client_id = None;
        assert!(check(&fields, true, Some("app-secret"), None));
        fields.grant = "preqin".into();
        assert!(!check(&fields, false, None, None));
        fields.grant = "refresh_token".into();
        fields.foreign_id = Some("bcr_x".into());
        assert!(!check(&fields, true, Some("s"), Some("other")));
    }
}
