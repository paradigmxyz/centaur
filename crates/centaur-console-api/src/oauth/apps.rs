//! `/oauth_apps` (bearer and console), `/api/v1/console/oauth_providers`,
//! the app's minted credentials and provider details, and one-click
//! presets.

use axum::{
    extract::{RawQuery, State},
    response::{IntoResponse, Response},
};
use centaur_console_db::{Ciphertext, Encryptor, decode_oid, oid};
use chrono::{DateTime, NaiveDateTime, Utc};
use http::{HeaderMap, StatusCode};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::{Postgres, QueryBuilder};

use super::{
    URL_SAFE_MESSAGE, blank, callback_url, credentials, error_response,
    exchange::{self},
    label_filter, nullable,
    providers::{self, Provider, REGISTRY},
    request_base, start_url, url_safe,
};
use crate::{
    ApiError, ApiResult, AppState, FieldErrors,
    auth::{BearerCaller, ConsoleCaller},
    extract::{Data, Path, Query, data},
    pagination::PageParams,
};

pub const PRESET_CONFLICT_MESSAGE: &str =
    "The preset conflicts with an existing OAuth app. Review its configuration.";
pub const PRESET_FAILED_MESSAGE: &str =
    "Could not set up the integration. Please try again or review the OAuth app configuration.";
pub const DEPENDENT_CREDENTIALS_MESSAGE: &str =
    "Cannot delete record because dependent broker credentials exist";

// --- reading ----------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    slug: String,
    description: Option<String>,
    labels: Value,
    provider: String,
    client_id: String,
    allowed_scopes: Value,
    provider_config: Value,
    enabled: bool,
    always_available: bool,
    shared: bool,
    client_secret_set: bool,
    credentials_count: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const SELECT: &str = "SELECT oa.id, oa.slug, oa.description, oa.labels, oa.provider, oa.client_id, \
    oa.allowed_scopes, oa.provider_config, oa.enabled, oa.always_available, oa.shared, \
    (oa.client_secret IS NOT NULL) AS client_secret_set, \
    (SELECT count(*) FROM broker_credentials WHERE oauth_app_id = oa.id) AS credentials_count, \
    oa.created_at AT TIME ZONE 'UTC' AS created_at, oa.updated_at AT TIME ZONE 'UTC' AS updated_at \
    FROM oauth_apps oa";

/// The JSON of an app. The console's adds what the page needs to set the app
/// up with its IdP; neither ever carries the client secret.
fn payload(row: &Row, console_base: Option<&str>) -> Result<Value, ApiError> {
    let mut payload = json!({
        "id": oid("oap", row.id),
        "slug": row.slug,
        "description": row.description,
        "labels": row.labels,
        "provider": row.provider,
        "client_id": row.client_id,
        "allowed_scopes": row.allowed_scopes,
        "provider_config": row.provider_config,
        "enabled": row.enabled,
        "always_available": row.always_available,
        "shared": row.shared,
        "created_at": row.created_at,
        "updated_at": row.updated_at,
    });
    if let (Some(base), Some(payload)) = (console_base, payload.as_object_mut()) {
        payload.insert("client_secret_set".into(), row.client_secret_set.into());
        payload.insert("redirect_uri".into(), callback_url(base, &row.slug)?.into());
        payload.insert("start_url".into(), start_url(base, &row.slug)?.into());
        payload.insert("credentials_count".into(), row.credentials_count.into());
    }
    Ok(payload)
}

fn app_id(id: &str) -> Result<i64, ApiError> {
    decode_oid("oap", id).ok_or_else(ApiError::not_found)
}

async fn find(state: &AppState, id: &str) -> Result<Row, ApiError> {
    sqlx::query_as(&format!("{SELECT} WHERE oa.id = $1"))
        .bind(app_id(id)?)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(ApiError::not_found)
}

async fn find_by_id(state: &AppState, id: i64) -> Result<Row, ApiError> {
    sqlx::query_as(&format!("{SELECT} WHERE oa.id = $1"))
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(ApiError::not_found)
}

async fn index(
    state: &AppState,
    raw: &RawQuery,
    page: PageParams,
    headers: &HeaderMap,
    console: bool,
) -> ApiResult {
    let labels = label_filter(raw);
    let filter = (!labels.is_empty()).then_some(Value::Object(labels));
    let page = page.resolve();
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM oauth_apps WHERE ($1::jsonb IS NULL OR labels @> $1)",
    )
    .bind(&filter)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} WHERE ($1::jsonb IS NULL OR oa.labels @> $1) \
         ORDER BY oa.created_at, oa.id LIMIT $2 OFFSET $3"
    ))
    .bind(&filter)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    let base = console.then(|| request_base(state, headers));
    let rows = rows
        .iter()
        .map(|row| payload(row, base.as_deref()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(axum::Json(json!({"data": rows, "meta": page.meta(total)})).into_response())
}

pub async fn bearer_index(
    State(state): State<AppState>,
    _caller: BearerCaller,
    headers: HeaderMap,
    raw: RawQuery,
    Query(page): Query<PageParams>,
) -> ApiResult {
    index(&state, &raw, page, &headers, false).await
}

pub async fn console_index(
    State(state): State<AppState>,
    _caller: ConsoleCaller,
    headers: HeaderMap,
    raw: RawQuery,
    Query(page): Query<PageParams>,
) -> ApiResult {
    index(&state, &raw, page, &headers, true).await
}

pub async fn bearer_show(
    State(state): State<AppState>,
    _caller: BearerCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let row = find(&state, &id).await?;
    Ok(data(payload(&row, None)?).into_response())
}

pub async fn console_show(
    State(state): State<AppState>,
    _caller: ConsoleCaller,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let row = find(&state, &id).await?;
    let base = request_base(&state, &headers);
    Ok(data(payload(&row, Some(&base))?).into_response())
}

/// `GET /oauth_apps/lookup/{slug}`.
pub async fn bearer_lookup(
    State(state): State<AppState>,
    _caller: BearerCaller,
    Path(slug): Path<String>,
) -> ApiResult {
    let row: Row = sqlx::query_as(&format!("{SELECT} WHERE oa.slug = $1"))
        .bind(&slug)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(data(payload(&row, None)?).into_response())
}

// --- writing ----------------------------------------------------------------------

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct Input {
    slug: Option<String>,
    #[serde(deserialize_with = "nullable")]
    description: Option<Option<String>>,
    provider: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    enabled: Option<bool>,
    always_available: Option<bool>,
    shared: Option<bool>,
    labels: Option<Map<String, Value>>,
    allowed_scopes: Option<Vec<String>>,
    provider_config: Option<Map<String, Value>>,
}

impl std::fmt::Debug for Input {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Input")
            .field("slug", &self.slug)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// An app's writable state.
#[derive(Clone)]
struct Fields {
    slug: String,
    description: Option<String>,
    provider: String,
    client_id: String,
    client_secret: Option<String>,
    enabled: bool,
    always_available: bool,
    shared: bool,
    labels: Map<String, Value>,
    allowed_scopes: Vec<String>,
    provider_config: Map<String, Value>,
}

impl std::fmt::Debug for Fields {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fields")
            .field("slug", &self.slug)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

#[derive(sqlx::FromRow)]
struct Stored {
    id: i64,
    slug: String,
    description: Option<String>,
    provider: String,
    client_id: String,
    client_secret: Option<Ciphertext>,
    enabled: bool,
    always_available: bool,
    shared: bool,
    labels: Value,
    allowed_scopes: Value,
    provider_config: Value,
}

const SELECT_STORED: &str = "SELECT id, slug, description, provider, client_id, client_secret, \
    enabled, always_available, shared, labels, allowed_scopes, provider_config FROM oauth_apps";

impl Fields {
    fn new() -> Self {
        Self {
            slug: String::new(),
            description: None,
            provider: String::new(),
            client_id: String::new(),
            client_secret: None,
            enabled: true,
            always_available: false,
            shared: false,
            labels: Map::new(),
            allowed_scopes: Vec::new(),
            provider_config: Map::new(),
        }
    }

    fn load(row: &Stored, encryptor: &Encryptor) -> Result<Self, ApiError> {
        Ok(Self {
            slug: row.slug.clone(),
            description: row.description.clone(),
            provider: row.provider.clone(),
            client_id: row.client_id.clone(),
            client_secret: encryptor.decrypt_optional(row.client_secret.as_ref())?,
            enabled: row.enabled,
            always_available: row.always_available,
            shared: row.shared,
            labels: row.labels.as_object().cloned().unwrap_or_default(),
            allowed_scopes: serde_json::from_value(row.allowed_scopes.clone()).unwrap_or_default(),
            provider_config: row.provider_config.as_object().cloned().unwrap_or_default(),
        })
    }

    /// Applies a request. A blank slug or client secret keeps the stored one.
    fn assign(&mut self, input: Input) {
        if let Some(slug) = input.slug.filter(|slug| !slug.trim().is_empty()) {
            self.slug = slug;
        }
        if let Some(description) = input.description {
            self.description = description;
        }
        if let Some(provider) = input.provider {
            self.provider = provider;
        }
        if let Some(client_id) = input.client_id {
            self.client_id = client_id;
        }
        if let Some(secret) = input
            .client_secret
            .filter(|secret| !secret.trim().is_empty())
        {
            self.client_secret = Some(secret);
        }
        if let Some(enabled) = input.enabled {
            self.enabled = enabled;
        }
        if let Some(value) = input.always_available {
            self.always_available = value;
        }
        if let Some(value) = input.shared {
            self.shared = value;
        }
        if let Some(labels) = input.labels {
            self.labels = labels;
        }
        if let Some(scopes) = input.allowed_scopes {
            self.allowed_scopes = scopes;
        }
        if let Some(config) = input.provider_config {
            self.provider_config = config;
        }
    }

    fn validate(&self, stored_shared: Option<bool>) -> FieldErrors {
        let mut errors = FieldErrors::new();
        if self.slug.trim().is_empty() {
            errors.add("slug", "can't be blank");
        } else if !url_safe(&self.slug) {
            errors.add("slug", URL_SAFE_MESSAGE);
        } else if self.slug.starts_with("oap_") {
            errors.add(
                "slug",
                "must not start with \"oap_\", which is reserved for opaque ids",
            );
        }
        if providers::ProviderKey::parse(&self.provider).is_none() {
            errors.add("provider", "is not a supported provider");
        }
        if self.client_id.trim().is_empty() {
            errors.add("client_id", "can't be blank");
        }
        if blank(self.client_secret.as_deref()) {
            errors.add("client_secret", "can't be blank");
        }
        if self.allowed_scopes.is_empty()
            || self
                .allowed_scopes
                .iter()
                .any(|scope| scope.trim().is_empty())
        {
            errors.add(
                "allowed_scopes",
                "must be a non-empty array of non-blank strings",
            );
        }
        if stored_shared.is_some_and(|shared| shared != self.shared) {
            errors.add("shared", "cannot be changed after creation");
        }
        if self.shared && self.always_available {
            errors.add(
                "shared",
                "cannot be always available to the connecting user",
            );
        }
        if self.provider == "custom" {
            for message in providers::custom_config_errors(&self.provider_config) {
                errors.add("provider_config", message);
            }
        } else if !self.provider_config.is_empty() {
            errors.add(
                "provider_config",
                "is only supported by the custom provider",
            );
        }
        errors
    }
}

fn slug_taken(error: sqlx::Error) -> ApiError {
    ApiError::unique(error, "slug", "has already been taken")
}

async fn insert(
    state: &AppState,
    fields: &Fields,
    created_by: i64,
    now: NaiveDateTime,
) -> Result<i64, ApiError> {
    sqlx::query_scalar(
        "INSERT INTO oauth_apps (slug, description, provider, client_id, client_secret, enabled, \
         always_available, shared, labels, allowed_scopes, provider_config, created_by_id, \
         created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $13) RETURNING id",
    )
    .bind(&fields.slug)
    .bind(&fields.description)
    .bind(&fields.provider)
    .bind(&fields.client_id)
    .bind(
        state
            .encryptor
            .encrypt_optional(fields.client_secret.as_deref())?,
    )
    .bind(fields.enabled)
    .bind(fields.always_available)
    .bind(fields.shared)
    .bind(Value::Object(fields.labels.clone()))
    .bind(json!(fields.allowed_scopes))
    .bind(Value::Object(fields.provider_config.clone()))
    .bind(created_by)
    .bind(now)
    .fetch_one(&state.db)
    .await
    .map_err(slug_taken)
}

/// Updates only the columns that differ.
async fn update(
    state: &AppState,
    id: i64,
    old: &Fields,
    new: &Fields,
    now: NaiveDateTime,
) -> Result<(), ApiError> {
    let mut query = QueryBuilder::<Postgres>::new("UPDATE oauth_apps SET ");
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
    set!("slug", slug);
    set!("description", description);
    set!("provider", provider);
    set!("client_id", client_id);
    set!("enabled", enabled);
    set!("always_available", always_available);
    set!("shared", shared);
    if old.client_secret != new.client_secret {
        changed = true;
        sets.push("client_secret = ").push_bind_unseparated(
            state
                .encryptor
                .encrypt_optional(new.client_secret.as_deref())?,
        );
    }
    if old.labels != new.labels {
        changed = true;
        sets.push("labels = ")
            .push_bind_unseparated(Value::Object(new.labels.clone()));
    }
    if old.allowed_scopes != new.allowed_scopes {
        changed = true;
        sets.push("allowed_scopes = ")
            .push_bind_unseparated(json!(new.allowed_scopes));
    }
    if old.provider_config != new.provider_config {
        changed = true;
        sets.push("provider_config = ")
            .push_bind_unseparated(Value::Object(new.provider_config.clone()));
    }
    if changed {
        sets.push("updated_at = ").push_bind_unseparated(now);
        query.push(" WHERE id = ").push_bind(id);
        query.build().execute(&state.db).await.map_err(slug_taken)?;
    }
    Ok(())
}

/// Creates (`existing: None`) or updates an app; returns its id and whether
/// it was created.
async fn save(
    state: &AppState,
    caller_id: i64,
    existing: Option<Stored>,
    input: Input,
) -> Result<(i64, bool), ApiError> {
    let now = state.now().naive_utc();
    let old = match &existing {
        Some(row) => Fields::load(row, &state.encryptor)?,
        None => Fields::new(),
    };
    let mut fields = old.clone();
    fields.assign(input);
    fields
        .validate(existing.as_ref().map(|row| row.shared))
        .finish()?;
    match existing {
        None => Ok((insert(state, &fields, caller_id, now).await?, true)),
        Some(row) => {
            update(state, row.id, &old, &fields, now).await?;
            Ok((row.id, false))
        }
    }
}

/// `PATCH/PUT /oauth_apps/{id}`: `id` is an opaque id (which must exist) or a
/// slug (created when it does not exist yet).
async fn upsert(
    state: &AppState,
    caller_id: i64,
    id: &str,
    mut input: Input,
) -> Result<(i64, bool), ApiError> {
    let existing: Option<Stored> = if id.starts_with("oap_") {
        let row_id = app_id(id)?;
        Some(
            sqlx::query_as(&format!("{SELECT_STORED} WHERE id = $1"))
                .bind(row_id)
                .fetch_optional(&state.db)
                .await?
                .ok_or_else(ApiError::not_found)?,
        )
    } else {
        sqlx::query_as(&format!("{SELECT_STORED} WHERE slug = $1"))
            .bind(id)
            .fetch_optional(&state.db)
            .await?
    };
    if existing.is_none() {
        input.slug = Some(id.to_owned());
    }
    save(state, caller_id, existing, input).await
}

fn status(created: bool) -> StatusCode {
    if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    }
}

pub async fn bearer_create(
    State(state): State<AppState>,
    BearerCaller(auth): BearerCaller,
    Data(input): Data<Input>,
) -> ApiResult {
    let (id, _) = save(&state, auth.user.id, None, input).await?;
    let row = find_by_id(&state, id).await?;
    Ok((StatusCode::CREATED, data(payload(&row, None)?)).into_response())
}

pub async fn console_create(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    headers: HeaderMap,
    Data(input): Data<Input>,
) -> ApiResult {
    let (id, _) = save(&state, user.id, None, input).await?;
    let row = find_by_id(&state, id).await?;
    let base = request_base(&state, &headers);
    Ok((StatusCode::CREATED, data(payload(&row, Some(&base))?)).into_response())
}

pub async fn bearer_update(
    State(state): State<AppState>,
    BearerCaller(auth): BearerCaller,
    Path(id): Path<String>,
    Data(input): Data<Input>,
) -> ApiResult {
    let (id, created) = upsert(&state, auth.user.id, &id, input).await?;
    let row = find_by_id(&state, id).await?;
    Ok((status(created), data(payload(&row, None)?)).into_response())
}

pub async fn console_update(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    headers: HeaderMap,
    Path(id): Path<String>,
    Data(input): Data<Input>,
) -> ApiResult {
    let (id, created) = upsert(&state, user.id, &id, input).await?;
    let row = find_by_id(&state, id).await?;
    let base = request_base(&state, &headers);
    Ok((status(created), data(payload(&row, Some(&base))?)).into_response())
}

/// `DELETE`: refused (409) while minted credentials exist.
pub async fn destroy(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = app_id(&id)?;
    match sqlx::query("DELETE FROM oauth_apps WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await
    {
        Ok(result) if result.rows_affected() == 0 => Err(ApiError::not_found()),
        Ok(_) => Ok(StatusCode::NO_CONTENT.into_response()),
        Err(error)
            if error
                .as_database_error()
                .is_some_and(|error| error.is_foreign_key_violation()) =>
        {
            Ok(error_response(
                StatusCode::CONFLICT,
                DEPENDENT_CREDENTIALS_MESSAGE,
            ))
        }
        Err(error) => Err(error.into()),
    }
}

// --- the console's views of an app -------------------------------------------------

#[derive(sqlx::FromRow)]
struct Minted {
    id: i64,
    foreign_id: Option<String>,
    name: Option<String>,
    dead: bool,
    last_refresh: Option<DateTime<Utc>>,
    provider_email: Option<String>,
    external_user_key: Option<String>,
    created_at: DateTime<Utc>,
}

/// `GET /oauth_apps/{id}/credentials`: the credentials minted through the
/// app, oldest first, without token material.
pub async fn console_credentials(
    State(state): State<AppState>,
    _caller: ConsoleCaller,
    Path(id): Path<String>,
    raw: RawQuery,
    Query(page): Query<PageParams>,
) -> ApiResult {
    let app = app_id(&id)?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM oauth_apps WHERE id = $1)")
        .bind(app)
        .fetch_one(&state.db)
        .await?;
    if !exists {
        return Err(ApiError::not_found());
    }
    let labels = label_filter(&raw);
    let filter = (!labels.is_empty()).then_some(Value::Object(labels));
    let page = page.resolve();
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM broker_credentials \
         WHERE oauth_app_id = $1 AND ($2::jsonb IS NULL OR labels @> $2)",
    )
    .bind(app)
    .bind(&filter)
    .fetch_one(&state.db)
    .await?;
    let rows: Vec<Minted> = sqlx::query_as(
        "SELECT id, foreign_id, name, dead, last_refresh AT TIME ZONE 'UTC' AS last_refresh, \
         provider_email, external_user_key, created_at AT TIME ZONE 'UTC' AS created_at \
         FROM broker_credentials WHERE oauth_app_id = $1 AND ($2::jsonb IS NULL OR labels @> $2) \
         ORDER BY created_at, id LIMIT $3 OFFSET $4",
    )
    .bind(app)
    .bind(&filter)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    let data: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": oid("bcr", row.id),
                "foreign_id": row.foreign_id,
                "name": row.name,
                "status": credentials::status(row.dead, row.last_refresh),
                "provider_email": row.provider_email,
                "external_user_key": row.external_user_key,
                "created_at": row.created_at,
            })
        })
        .collect();
    Ok(axum::Json(json!({"data": data, "meta": page.meta(total)})).into_response())
}

#[derive(sqlx::FromRow)]
struct Account {
    id: i64,
    provider_subject: Option<String>,
    dead: bool,
    expires_at: Option<NaiveDateTime>,
    access_token: Option<Ciphertext>,
    static_secret_id: Option<i64>,
}

/// `GET /oauth_apps/{id}/provider_details`: for a shared app, its account and
/// (where the provider describes it) live details of that account.
pub async fn console_provider_details(
    State(state): State<AppState>,
    _caller: ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    #[derive(sqlx::FromRow)]
    struct AppRow {
        slug: String,
        provider: String,
        provider_config: Value,
        shared: bool,
    }
    let app: AppRow = sqlx::query_as(
        "SELECT slug, provider, provider_config, shared FROM oauth_apps WHERE id = $1",
    )
    .bind(app_id(&id)?)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let account: Option<Account> = if app.shared {
        sqlx::query_as(
            "SELECT bc.id, bc.provider_subject, bc.dead, bc.expires_at, bc.access_token, \
             (SELECT id FROM static_secrets WHERE broker_credential_id = bc.id ORDER BY id LIMIT 1) \
               AS static_secret_id \
             FROM broker_credentials bc WHERE bc.oauth_app_id = (SELECT id FROM oauth_apps WHERE slug = $1) \
             ORDER BY bc.created_at, bc.id LIMIT 1",
        )
        .bind(&app.slug)
        .fetch_optional(&state.db)
        .await?
    } else {
        None
    };
    let Some(account) = account else {
        return Ok(
            data(json!({"provider": app.provider, "credential": null, "details": {}}))
                .into_response(),
        );
    };
    let details = match Provider::for_app(&app.provider, &app.slug, &app.provider_config) {
        Some(provider) if provider.key == providers::ProviderKey::Mercator => {
            let token = state
                .encryptor
                .decrypt_optional(account.access_token.as_ref())
                .ok()
                .flatten()
                .filter(|token| !token.trim().is_empty());
            let live = !account.dead
                && account
                    .expires_at
                    .is_none_or(|expires_at| expires_at > state.now().naive_utc());
            match token.filter(|_| live) {
                Some(token) => {
                    provider
                        .details(&state, &token, account.provider_subject.as_deref())
                        .await
                }
                None => json!({}),
            }
        }
        _ => json!({}),
    };
    Ok(data(json!({
        "provider": app.provider,
        "credential": {
            "id": oid("bcr", account.id),
            "provider_subject": account.provider_subject,
            "static_secret_id": account.static_secret_id.map(|id| oid("ssr", id)),
        },
        "details": details,
    }))
    .into_response())
}

/// `GET /oauth_providers`.
pub async fn providers_index(State(_state): State<AppState>, _caller: ConsoleCaller) -> ApiResult {
    let data_rows: Vec<Value> = REGISTRY
        .iter()
        .map(|key| {
            let provider = Provider::registered(*key);
            let custom = *key == providers::ProviderKey::Custom;
            let keys = |list: &[&str]| -> Vec<String> {
                if custom {
                    list.iter().map(|key| (*key).to_owned()).collect()
                } else {
                    Vec::new()
                }
            };
            json!({
                "key": key.as_str(),
                "display_name": provider.display_name(),
                "presettable": provider.presettable(),
                "config_keys": keys(&providers::CUSTOM_CONFIG_KEYS),
                "optional_config_keys": keys(&providers::CUSTOM_OPTIONAL_CONFIG_KEYS),
            })
        })
        .collect();
    Ok(data(data_rows).into_response())
}

// --- presets ----------------------------------------------------------------------

/// The presettable providers that have no app yet.
pub async fn available_presets(state: &AppState) -> Result<Vec<Provider>, ApiError> {
    let configured: Vec<String> = sqlx::query_scalar("SELECT DISTINCT provider FROM oauth_apps")
        .fetch_all(&state.db)
        .await?;
    Ok(REGISTRY
        .iter()
        .map(|key| Provider::registered(*key))
        .filter(|provider| {
            provider.presettable() && !configured.iter().any(|name| name == provider.key_str())
        })
        .collect())
}

#[derive(sqlx::FromRow)]
struct PresetApp {
    id: i64,
    slug: String,
    provider: String,
    shared: bool,
    enabled: bool,
}

async fn preset_app(state: &AppState, slug: &str) -> Result<Option<PresetApp>, ApiError> {
    Ok(
        sqlx::query_as(
            "SELECT id, slug, provider, shared, enabled FROM oauth_apps WHERE slug = $1",
        )
        .bind(slug)
        .fetch_optional(&state.db)
        .await?,
    )
}

/// Registers a client with the provider and creates the preset's app. A
/// concurrent request that created it first wins.
async fn register_and_create(
    state: &AppState,
    provider: &Provider,
    user_id: i64,
    base: &str,
) -> Result<PresetApp, Response> {
    let preset = provider.preset().expect("presettable provider");
    let failed = || error_response(StatusCode::BAD_GATEWAY, PRESET_FAILED_MESSAGE);
    let redirect = callback_url(base, preset.slug).map_err(IntoResponse::into_response)?;
    let registration = exchange::register(
        state,
        &provider
            .registration_endpoint()
            .expect("registration endpoint"),
        &redirect,
        &preset.allowed_scopes.join(" "),
        provider.auth_method(),
    )
    .await
    .map_err(|_| failed())?;
    let now = state.now().naive_utc();
    let secret = state
        .encryptor
        .encrypt(&registration.client_secret)
        .map_err(|error| ApiError::from(error).into_response())?;
    let inserted: Option<i64> = sqlx::query_scalar(
        "INSERT INTO oauth_apps (slug, description, provider, client_id, client_secret, enabled, \
         shared, allowed_scopes, created_by_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, TRUE, $6, $7, $8, $9, $9) \
         ON CONFLICT (slug) DO NOTHING RETURNING id",
    )
    .bind(preset.slug)
    .bind(preset.description)
    .bind(provider.key_str())
    .bind(&registration.client_id)
    .bind(secret)
    .bind(preset.shared)
    .bind(json!(preset.allowed_scopes))
    .bind(user_id)
    .bind(now)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| ApiError::from(error).into_response())?;
    let _ = inserted;
    match preset_app(state, preset.slug).await {
        Ok(Some(app)) => Ok(app),
        Ok(None) => Err(failed()),
        Err(error) => Err(error.into_response()),
    }
}

/// `POST /oauth_apps/presets/{provider}`: the preset's app (created when its
/// slug is free) and where to start its consent flow.
pub async fn console_preset(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    headers: HeaderMap,
    Path(provider): Path<String>,
) -> ApiResult {
    let Some(provider) = Provider::fetch(&provider).filter(Provider::presettable) else {
        return Err(ApiError::NotFound("no preset for this provider".into()));
    };
    let preset = provider.preset().expect("presettable provider");
    let base = request_base(&state, &headers);
    let app = match preset_app(&state, preset.slug).await? {
        Some(app) => app,
        None => match register_and_create(&state, &provider, user.id, &base).await {
            Ok(app) => app,
            Err(response) => return Ok(response),
        },
    };
    if app.provider != provider.key_str() || app.shared != preset.shared || !app.enabled {
        return Ok(error_response(
            StatusCode::CONFLICT,
            PRESET_CONFLICT_MESSAGE,
        ));
    }
    Ok(data(json!({
        "id": oid("oap", app.id),
        "slug": app.slug,
        "start_url": start_url(&base, &app.slug)?,
    }))
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(value: Value) -> Input {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn validation_covers_the_app_rules() {
        let mut fields = Fields::new();
        fields.assign(input(json!({
            "slug": "google", "provider": "google", "client_id": "cid", "client_secret": "s",
            "allowed_scopes": ["openid"]
        })));
        assert!(fields.validate(None).is_empty());
        for (patch, field) in [
            (json!({"slug": "oap_x"}), "slug"),
            (json!({"slug": "not safe"}), "slug"),
            (json!({"provider": "nope"}), "provider"),
            (json!({"allowed_scopes": []}), "allowed_scopes"),
            (json!({"allowed_scopes": [" "]}), "allowed_scopes"),
            (json!({"shared": true, "always_available": true}), "shared"),
            (
                json!({"provider_config": {"resource": "https://x.example"}}),
                "provider_config",
            ),
        ] {
            let mut changed = fields.clone();
            changed.assign(input(patch));
            assert!(!changed.validate(None).is_empty(), "{field}");
        }
        let mut shared = fields.clone();
        shared.assign(input(json!({"shared": true})));
        assert!(!shared.validate(Some(false)).is_empty());
        // Blank slug and secret keep the stored values.
        let mut kept = fields.clone();
        kept.assign(input(json!({"slug": " ", "client_secret": ""})));
        assert_eq!(
            (kept.slug.as_str(), kept.client_secret.as_deref()),
            ("google", Some("s"))
        );
    }

    #[test]
    fn debug_hides_the_client_secret() {
        let mut fields = Fields::new();
        fields.client_secret = Some("SECRET-VALUE".into());
        assert!(!format!("{fields:?}").contains("SECRET-VALUE"));
        let input = input(json!({"client_secret": "SECRET-VALUE"}));
        assert!(!format!("{input:?}").contains("SECRET-VALUE"));
    }
}
