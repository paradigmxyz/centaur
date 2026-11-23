//! OAuth app, provider, preset, integration, and provider-detail handlers.

use std::collections::HashMap;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use centaur_console_db::{
    ConsoleUser, FieldErrors,
    areas::oauth::{
        BrokerCredential, NewOauthApp, OauthApp, OauthEncryption, URL_SAFE_MESSAGE,
        all_oauth_app_providers, all_oauth_app_slugs, connections_for_user,
        disconnect_broker_credential, enabled_oauth_apps, find_oauth_app_by_slug, first_credential,
        insert_oauth_app, oauth_app_credential_counts, oauth_app_credentials_count,
        update_oauth_app, url_safe, user_has_ssh_key, wrapping_secret_for_credential,
    },
    model::{
        self, BrokerCredential as BrokerCredentialModel, ConsoleModel, OauthApp as OauthAppModel,
        StaticSecret as StaticSecretModel,
    },
    timestamp,
    validation::Validation,
};
use garde::Validate as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use time::PrimitiveDateTime;

use super::{
    Patch, app_by_id, app_by_oid, blank, config_failure, created, credential_by_oid, document,
    is_oid, label_filter, oauth_callback_redirect_uri, oauth_start_url, pool, present,
    providers::{
        self, CUSTOM_CONFIG_KEYS, CUSTOM_KEY, CUSTOM_OPTIONAL_CONFIG_KEYS, OauthAppCredential,
        PROVIDER_KEYS, Preset, Provider, ProviderConfig, RegistrationRequest, registration_scope,
    },
};
use crate::{
    AppState,
    console::{
        areas::auth_pages::console_config,
        guard::BearerUser,
        http::{
            ConsoleError, DataPayload, ListParams, ListSource, bind, list, list_response, list_rows,
        },
        request::{ConsoleBody, ConsoleData},
    },
    console_session::ConsoleSessionConfig,
};

/// One OAuth app document write. Unknown fields are ignored; a field of the
/// wrong JSON type is a validation error instead of a strong-parameter cast.
#[derive(Debug, Default, Deserialize)]
pub(super) struct OauthAppInput {
    #[serde(default)]
    slug: Patch<String>,
    #[serde(default)]
    description: Patch<String>,
    #[serde(default)]
    labels: Option<Map<String, Value>>,
    #[serde(default)]
    provider: Patch<String>,
    #[serde(default)]
    client_id: Patch<String>,
    #[serde(default)]
    client_secret: Patch<String>,
    #[serde(default)]
    allowed_scopes: Option<Vec<String>>,
    #[serde(default)]
    provider_config: Option<Map<String, Value>>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    always_available: Option<bool>,
    #[serde(default)]
    shared: Option<bool>,
    #[serde(default)]
    owner_only: Option<bool>,
}

/// The preset endpoint reads an optional `data` object with one input.
#[derive(Debug, Default, Deserialize)]
pub(super) struct PresetBody {
    #[serde(default)]
    data: Option<PresetInput>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct PresetInput {
    #[serde(default)]
    resource_url: Option<String>,
}

/// The app list: the label filter binds `$1`, which the shared list helper
/// counts and pages over.
const APP_LIST: ListSource = ListSource::new(
    centaur_console_db::areas::oauth::APP_LIST_SELECT,
    centaur_console_db::areas::oauth::APP_LIST_FROM,
    centaur_console_db::areas::oauth::APP_LIST_ORDER,
    1,
);

/// The credentials of one app: the app id binds `$1`, the label filter `$2`.
const APP_CREDENTIAL_LIST: ListSource = ListSource::new(
    centaur_console_db::areas::oauth::APP_CREDENTIAL_LIST_SELECT,
    centaur_console_db::areas::oauth::APP_CREDENTIAL_LIST_FROM,
    centaur_console_db::areas::oauth::APP_CREDENTIAL_LIST_ORDER,
    2,
);

/// The `data` object of an OAuth app response.
#[derive(Serialize)]
struct AppPayload {
    id: String,
    slug: String,
    description: Option<String>,
    labels: Map<String, Value>,
    provider: String,
    client_id: String,
    allowed_scopes: Vec<String>,
    provider_config: Map<String, Value>,
    enabled: bool,
    always_available: bool,
    shared: bool,
    owner_only: bool,
    #[serde(with = "timestamp::primitive")]
    created_at: PrimitiveDateTime,
    #[serde(with = "timestamp::primitive")]
    updated_at: PrimitiveDateTime,
}

impl From<&OauthApp> for AppPayload {
    fn from(app: &OauthApp) -> Self {
        Self {
            id: OauthAppModel::oid(app.id),
            slug: app.slug.clone(),
            description: app.description.clone(),
            labels: app.labels.clone(),
            provider: app.provider.clone(),
            client_id: app.client_id.clone(),
            allowed_scopes: app.allowed_scopes.clone(),
            provider_config: app.provider_config.clone(),
            enabled: app.enabled,
            always_available: app.always_available,
            shared: app.shared,
            owner_only: app.owner_only,
            created_at: app.created_at,
            updated_at: app.updated_at,
        }
    }
}

/// The console payload adds the presence-only client secret, the public URLs,
/// and the app's credential count.
#[derive(Serialize)]
struct ConsoleAppPayload {
    #[serde(flatten)]
    app: AppPayload,
    client_secret_set: bool,
    redirect_uri: String,
    start_url: String,
    credentials_count: i64,
}

fn payload(app: &OauthApp) -> AppPayload {
    AppPayload::from(app)
}

fn console_app_payload(
    app: &OauthApp,
    config: &ConsoleSessionConfig,
    credentials_count: i64,
) -> ConsoleAppPayload {
    ConsoleAppPayload {
        client_secret_set: app
            .client_secret
            .as_deref()
            .is_some_and(|secret| !secret.is_empty()),
        redirect_uri: oauth_callback_redirect_uri(config, &app.slug),
        start_url: oauth_start_url(config, &app.slug),
        credentials_count,
        app: AppPayload::from(app),
    }
}

/// The credential summary `GET /oauth_apps/:id/credentials` renders.
#[derive(Serialize)]
struct MintedCredentialPayload {
    id: String,
    foreign_id: Option<String>,
    name: Option<String>,
    status: &'static str,
    provider_email: Option<String>,
    external_user_key: Option<String>,
    #[serde(with = "timestamp::primitive")]
    created_at: PrimitiveDateTime,
}

impl From<&BrokerCredential> for MintedCredentialPayload {
    fn from(credential: &BrokerCredential) -> Self {
        Self {
            id: BrokerCredentialModel::oid(credential.id),
            foreign_id: credential.foreign_id.clone(),
            name: credential.name.clone(),
            status: credential.status(),
            provider_email: credential.provider_email.clone(),
            external_user_key: credential.external_user_key.clone(),
            created_at: credential.created_at,
        }
    }
}

/// One entry of `GET /oauth_providers`.
#[derive(Serialize)]
pub(super) struct ProviderPayload {
    key: &'static str,
    display_name: String,
    presettable: bool,
    config_keys: Vec<&'static str>,
    optional_config_keys: Vec<&'static str>,
}

/// `GET /oauth_apps/:id/provider_details`.
#[derive(Serialize)]
struct ProviderDetailsPayload {
    provider: String,
    credential: Option<ProviderCredentialPayload>,
    details: Value,
}

#[derive(Serialize)]
struct ProviderCredentialPayload {
    id: String,
    provider_subject: Option<String>,
    static_secret_id: Option<String>,
}

/// The `POST /oauth_apps/presets/:provider` document.
#[derive(Serialize)]
struct PresetPayload {
    id: String,
    slug: String,
    start_url: String,
}

/// One app of the sandbox index.
#[derive(Serialize)]
struct SandboxAppPayload {
    id: String,
    slug: String,
    description: Option<String>,
    labels: Map<String, Value>,
    provider: String,
    allowed_scopes: Vec<String>,
    owner_only: bool,
    resource: Option<String>,
    start_url: String,
}

/// `GET /integrations`.
#[derive(Serialize)]
struct IntegrationsPayload {
    apps: Vec<IntegrationAppPayload>,
    presets: Vec<IntegrationPresetPayload>,
    ssh_key: SshKeyPayload,
}

#[derive(Serialize)]
struct IntegrationAppPayload {
    id: String,
    slug: String,
    provider: String,
    description: Option<String>,
    shared: bool,
    always_available: bool,
    owner_only: bool,
    usage: &'static str,
    start_url: String,
    connection: Option<ConnectionPayload>,
}

#[derive(Serialize)]
struct ConnectionPayload {
    id: String,
    status: &'static str,
    can_disconnect: bool,
}

#[derive(Serialize)]
struct IntegrationPresetPayload {
    provider: &'static str,
    display_name: &'static str,
    description: String,
    inputs: Value,
}

#[derive(Serialize)]
struct SshKeyPayload {
    configured: bool,
}

/// `DELETE /integrations/connections/:id`.
#[derive(Serialize)]
struct DisconnectPayload {
    id: String,
    outcome: &'static str,
}

/// The write-side view of an app after applying the body over the existing row.
/// The field rules are garde attributes here because this merged record is what
/// the model validates; rules that need the stored row or a uniqueness lookup
/// stay in [`app_validation`].
#[derive(garde::Validate)]
struct AppWrite {
    #[garde(custom(present), custom(slug_format), custom(slug_not_reserved))]
    slug: String,
    #[garde(skip)]
    description: Option<String>,
    #[garde(skip)]
    labels: Map<String, Value>,
    #[garde(custom(provider_supported))]
    provider: String,
    #[garde(custom(present))]
    client_id: String,
    /// The plaintext supplied in this request, if any.
    #[garde(skip)]
    client_secret_plain: Option<String>,
    /// The encrypted value to store (the supplied secret or the stored one).
    #[garde(skip)]
    client_secret_stored: Option<String>,
    #[garde(custom(allowed_scopes_valid))]
    allowed_scopes: Vec<String>,
    #[garde(skip)]
    provider_config: Map<String, Value>,
    #[garde(skip)]
    enabled: bool,
    #[garde(skip)]
    always_available: bool,
    #[garde(skip)]
    shared: bool,
    #[garde(skip)]
    owner_only: bool,
    #[garde(skip)]
    shared_touched: Option<bool>,
    #[garde(skip)]
    owner_only_touched: Option<bool>,
    #[garde(skip)]
    created_by_id: i64,
}

impl AppWrite {
    fn new(created_by_id: i64, identity_slug: Option<&str>) -> Self {
        Self {
            slug: identity_slug.unwrap_or_default().to_owned(),
            description: None,
            labels: Map::new(),
            provider: String::new(),
            client_id: String::new(),
            client_secret_plain: None,
            client_secret_stored: None,
            allowed_scopes: Vec::new(),
            provider_config: Map::new(),
            enabled: true,
            always_available: false,
            shared: false,
            owner_only: false,
            shared_touched: None,
            owner_only_touched: None,
            created_by_id,
        }
    }

    fn from_existing(app: &OauthApp) -> Self {
        Self {
            slug: app.slug.clone(),
            description: app.description.clone(),
            labels: app.labels.clone(),
            provider: app.provider.clone(),
            client_id: app.client_id.clone(),
            client_secret_plain: None,
            client_secret_stored: app.client_secret.clone(),
            allowed_scopes: app.allowed_scopes.clone(),
            provider_config: app.provider_config.clone(),
            enabled: app.enabled,
            always_available: app.always_available,
            shared: app.shared,
            owner_only: app.owner_only,
            shared_touched: None,
            owner_only_touched: None,
            created_by_id: app.created_by_id,
        }
    }

    fn apply(&mut self, data: &OauthAppInput, encryption: &OauthEncryption) {
        // A PUT upsert by slug sets the slug before assignment; a blank body
        // value must not wipe it.
        match &data.slug {
            Patch::Value(value) if !blank(value) || blank(&self.slug) => {
                self.slug = value.clone();
            }
            Patch::Null if blank(&self.slug) => self.slug.clear(),
            Patch::Value(_) | Patch::Null | Patch::Missing => {}
        }

        match &data.description {
            Patch::Value(value) => self.description = Some(value.clone()),
            Patch::Null => self.description = None,
            Patch::Missing => {}
        }
        if let Some(labels) = &data.labels {
            self.labels = labels.clone();
        }
        match &data.provider {
            Patch::Value(value) => self.provider = value.clone(),
            Patch::Null => self.provider.clear(),
            Patch::Missing => {}
        }
        match &data.client_id {
            Patch::Value(value) => self.client_id = value.clone(),
            Patch::Null => self.client_id.clear(),
            Patch::Missing => {}
        }

        // client_secret is write-only: a blank or omitted value leaves the
        // stored secret in place.
        if let Patch::Value(secret) = &data.client_secret
            && !blank(secret)
        {
            self.client_secret_plain = Some(secret.clone());
            self.client_secret_stored = encryption.encrypt(secret).ok();
        }

        if let Some(scopes) = &data.allowed_scopes {
            self.allowed_scopes = scopes.clone();
        }
        if let Some(config) = &data.provider_config {
            self.provider_config = config.clone();
        }
        if let Some(value) = data.enabled {
            self.enabled = value;
        }
        if let Some(value) = data.shared {
            self.shared = value;
            self.shared_touched = Some(value);
        }
        if let Some(value) = data.owner_only {
            self.owner_only = value;
            self.owner_only_touched = Some(value);
        }
        if let Some(value) = data.always_available {
            self.always_available = value;
        }
    }

    fn new_app(&self) -> NewOauthApp {
        NewOauthApp {
            slug: self.slug.clone(),
            description: self.description.clone(),
            labels: self.labels.clone(),
            provider: self.provider.clone(),
            client_id: self.client_id.clone(),
            client_secret: self.client_secret_stored.clone().unwrap_or_default(),
            allowed_scopes: self.allowed_scopes.clone(),
            provider_config: self.provider_config.clone(),
            enabled: self.enabled,
            always_available: self.always_available,
            shared: self.shared,
            owner_only: self.owner_only,
            created_by_id: self.created_by_id,
        }
    }
}

const SLUG_RESERVED_PREFIX: &str = "oap_";
const SCOPES_MESSAGE: &str = "must be a non-empty array of non-blank strings";
const SHARED_CHANGED_MESSAGE: &str = "cannot be changed after creation";
const SHARED_ALWAYS_AVAILABLE_MESSAGE: &str = "cannot be always available to the connecting user";
const OWNER_ONLY_CHANGED_MESSAGE: &str = "cannot be changed after creation";
const OWNER_ONLY_SHARED_MESSAGE: &str = "cannot be combined with a shared app";

fn slug_format(value: &str, _: &()) -> garde::Result {
    if !blank(value) && !url_safe(value) {
        return Err(garde::Error::new(URL_SAFE_MESSAGE));
    }
    Ok(())
}

fn slug_not_reserved(value: &str, _: &()) -> garde::Result {
    if !blank(value) && value.starts_with(SLUG_RESERVED_PREFIX) {
        return Err(garde::Error::new(format!(
            "must not start with \"{SLUG_RESERVED_PREFIX}\", which is reserved for opaque ids"
        )));
    }
    Ok(())
}

fn provider_supported(value: &str, _: &()) -> garde::Result {
    if !PROVIDER_KEYS.contains(&value) {
        return Err(garde::Error::new("is not a supported provider"));
    }
    Ok(())
}

fn allowed_scopes_valid(value: &[String], _: &()) -> garde::Result {
    if value.is_empty() || value.iter().any(|scope| scope.trim().is_empty()) {
        return Err(garde::Error::new(SCOPES_MESSAGE));
    }
    Ok(())
}

/// The app rules that need the stored row, a uniqueness lookup, or the
/// provider config as a whole. Everything that is a rule of one merged field is
/// a garde attribute on [`AppWrite`].
fn app_validation(
    write: &AppWrite,
    existing_shared: Option<bool>,
    existing_owner_only: Option<bool>,
    slug_taken: bool,
) -> FieldErrors {
    let mut validation = Validation::new();
    validation.check(write);
    if !blank(&write.slug) && slug_taken {
        validation.add("slug", "has already been taken");
    }
    let secret = write
        .client_secret_plain
        .as_deref()
        .filter(|secret| !blank(secret))
        .or_else(|| {
            write
                .client_secret_stored
                .as_deref()
                .filter(|secret| !secret.is_empty())
        });
    if secret.is_none() {
        validation.add("client_secret", "can't be blank");
    }
    if existing_shared.is_some_and(|existing| {
        write
            .shared_touched
            .is_some_and(|shared| shared != existing)
    }) {
        validation.add("shared", SHARED_CHANGED_MESSAGE);
    }
    if write.shared && write.always_available {
        validation.add("shared", SHARED_ALWAYS_AVAILABLE_MESSAGE);
    }
    if existing_owner_only.is_some_and(|existing| {
        write
            .owner_only_touched
            .is_some_and(|owner_only| owner_only != existing)
    }) {
        validation.add("owner_only", OWNER_ONLY_CHANGED_MESSAGE);
    }
    if write.owner_only && write.shared {
        validation.add("owner_only", OWNER_ONLY_SHARED_MESSAGE);
    }
    let provider_config = ProviderConfig::new(&write.provider, &write.provider_config);
    if let Err(report) = provider_config.validate() {
        for (_, error) in report.into_inner() {
            validation.add("provider_config", error.message());
        }
    }
    validation.into_result().err().unwrap_or_default()
}

async fn validate_app(pool: &PgPool, write: &AppWrite, existing: Option<&OauthApp>) -> FieldErrors {
    // The uniqueness lookup needs the database.
    let slug_taken = !blank(&write.slug)
        && matches!(
            find_oauth_app_by_slug(pool, &write.slug).await,
            Ok(Some(other)) if existing.is_none_or(|existing| existing.id != other.id)
        );
    app_validation(
        write,
        existing.map(|existing| existing.shared),
        existing.map(|existing| existing.owner_only),
        slug_taken,
    )
}

/// Creates or updates an app from a body, then returns the persisted row.
async fn save_app(
    pool: &PgPool,
    data: &OauthAppInput,
    existing: Option<OauthApp>,
    identity_slug: Option<&str>,
    created_by_id: i64,
) -> Result<OauthApp, ConsoleError> {
    let encryption = OauthEncryption::from_env().ok_or_else(config_failure)?;
    let mut write = match &existing {
        Some(app) => AppWrite::from_existing(app),
        None => AppWrite::new(created_by_id, identity_slug),
    };
    write.apply(data, &encryption);
    let errors = validate_app(pool, &write, existing.as_ref()).await;
    if !errors.is_empty() {
        return Err(errors.into());
    }
    let new = write.new_app();
    // A secret supplied by this request replaces the stored one; otherwise the
    // stored value (or its absence) is written back unchanged.
    let client_secret = write.client_secret_stored.as_deref();
    match &existing {
        Some(app) => {
            update_oauth_app(pool, app.id, &new, client_secret).await?;
            app_by_id(pool, app.id).await
        }
        None => {
            let id = insert_oauth_app(pool, &new).await?;
            app_by_id(pool, id).await
        }
    }
}

pub(super) async fn console_index(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    let params = ListParams::from_query(&query)?;
    let pool = pool(&state)?;
    let (apps, total) = list_rows::<OauthApp, _>(&pool, &params, APP_LIST, |arguments| {
        bind(arguments, label_filter(&params))
    })
    .await?;
    let ids: Vec<i64> = apps.iter().map(|app| app.id).collect();
    let counts = oauth_app_credential_counts(&pool, &ids).await?;
    let data: Vec<ConsoleAppPayload> = apps
        .iter()
        .map(|app| {
            console_app_payload(
                app,
                console_config(&state),
                *counts.get(&app.id).unwrap_or(&0),
            )
        })
        .collect();
    Ok(Json(list_response(params.pagination, &data, total)).into_response())
}

pub(super) async fn console_show(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let app = app_by_oid(&pool, &id).await?;
    let count = oauth_app_credentials_count(&pool, app.id).await?;
    Ok(document(console_app_payload(&app, console_config(&state), count)).into_response())
}

pub(super) async fn bearer_show(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let app = app_by_oid(&pool, &id).await?;
    Ok(document(AppPayload::from(&app)).into_response())
}

pub(super) async fn bearer_lookup(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let app = find_oauth_app_by_slug(&pool, &slug)
        .await?
        .ok_or_else(OauthAppModel::not_found)?;
    Ok(document(AppPayload::from(&app)).into_response())
}

pub(super) async fn bearer_index(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    let params = ListParams::from_query(&query)?;
    let pool = pool(&state)?;
    let body = list::<OauthApp, AppPayload, _, _>(
        &pool,
        &params,
        APP_LIST,
        |arguments| bind(arguments, label_filter(&params)),
        payload,
    )
    .await?;
    Ok(Json(body).into_response())
}

pub(super) async fn console_create(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleData(data): ConsoleData<OauthAppInput>,
) -> Result<Response, ConsoleError> {
    create_app(&state, user.id, &data, true).await
}

pub(super) async fn bearer_create(
    State(state): State<AppState>,
    user: BearerUser,
    ConsoleData(data): ConsoleData<OauthAppInput>,
) -> Result<Response, ConsoleError> {
    create_app(&state, user.0.id, &data, false).await
}

async fn create_app(
    state: &AppState,
    user_id: i64,
    data: &OauthAppInput,
    console: bool,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let app = save_app(&pool, data, None, None, user_id).await?;
    if console {
        return Ok(created(console_app_payload(&app, console_config(state), 0)).into_response());
    }
    Ok(created(AppPayload::from(&app)).into_response())
}

pub(super) async fn console_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
    ConsoleData(data): ConsoleData<OauthAppInput>,
) -> Result<Response, ConsoleError> {
    update_app(&state, &id, user.id, &data, true).await
}

pub(super) async fn bearer_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: BearerUser,
    ConsoleData(data): ConsoleData<OauthAppInput>,
) -> Result<Response, ConsoleError> {
    update_app(&state, &id, user.0.id, &data, false).await
}

async fn update_app(
    state: &AppState,
    identifier: &str,
    user_id: i64,
    data: &OauthAppInput,
    console: bool,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let (existing, identity_slug) = if is_oid::<OauthAppModel>(identifier) {
        (Some(app_by_oid(&pool, identifier).await?), None)
    } else {
        match find_oauth_app_by_slug(&pool, identifier).await? {
            Some(app) => (Some(app), Some(identifier.to_owned())),
            None => (None, Some(identifier.to_owned())),
        }
    };
    let was_new = existing.is_none();
    let app = save_app(&pool, data, existing, identity_slug.as_deref(), user_id).await?;
    let status = if was_new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    if console {
        let count = oauth_app_credentials_count(&pool, app.id).await?;
        return Ok((
            status,
            document(console_app_payload(&app, console_config(state), count)),
        )
            .into_response());
    }
    Ok((status, document(AppPayload::from(&app))).into_response())
}

pub(super) async fn destroy(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let app = app_by_oid(&pool, &id).await?;
    if oauth_app_credentials_count(&pool, app.id).await? > 0 {
        return Err(ConsoleError::new(
            StatusCode::CONFLICT,
            "Cannot delete record because dependent broker credentials exist",
        ));
    }
    model::delete::<OauthAppModel, _>(&pool, app.id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(super) async fn console_credentials(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    let params = ListParams::from_query(&query)?;
    let pool = pool(&state)?;
    let app = app_by_oid(&pool, &id).await?;
    let (credentials, total) =
        list_rows::<BrokerCredential, _>(&pool, &params, APP_CREDENTIAL_LIST, |arguments| {
            bind(arguments, app.id)?;
            bind(arguments, label_filter(&params))
        })
        .await?;
    let data: Vec<MintedCredentialPayload> = credentials
        .iter()
        .map(MintedCredentialPayload::from)
        .collect();
    Ok(Json(list_response(params.pagination, &data, total)).into_response())
}

pub(super) async fn console_providers() -> Json<DataPayload<Vec<ProviderPayload>>> {
    let data: Vec<ProviderPayload> = Provider::registry_all()
        .iter()
        .map(|provider| {
            let custom = provider.key() == CUSTOM_KEY;
            ProviderPayload {
                key: provider.key(),
                display_name: provider.display_name(),
                presettable: provider.key() == providers::MERCATOR_KEY,
                config_keys: if custom {
                    CUSTOM_CONFIG_KEYS.to_vec()
                } else {
                    Vec::new()
                },
                optional_config_keys: if custom {
                    CUSTOM_OPTIONAL_CONFIG_KEYS.to_vec()
                } else {
                    Vec::new()
                },
            }
        })
        .collect();
    document(data)
}

pub(super) async fn console_provider_details(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let app = app_by_oid(&pool, &id).await?;
    let credential = if app.shared {
        first_credential(&pool, app.id).await?
    } else {
        None
    };
    let Some(credential) = credential else {
        return Ok(document(ProviderDetailsPayload {
            provider: app.provider.clone(),
            credential: None,
            details: json!({}),
        })
        .into_response());
    };
    let static_secret_id = wrapping_secret_for_credential(&pool, credential.id)
        .await?
        .map(|secret| StaticSecretModel::oid(secret.id));
    let payload = ProviderCredentialPayload {
        id: BrokerCredentialModel::oid(credential.id),
        provider_subject: credential.provider_subject.clone(),
        static_secret_id,
    };
    let details = provider_details_for(&app, &credential).await;
    Ok(document(ProviderDetailsPayload {
        provider: app.provider.clone(),
        credential: Some(payload),
        details,
    })
    .into_response())
}

async fn provider_details_for(app: &OauthApp, credential: &BrokerCredential) -> Value {
    provider_details(app, credential)
        .await
        .unwrap_or_else(|| json!({}))
}

async fn provider_details(app: &OauthApp, credential: &BrokerCredential) -> Option<Value> {
    if credential.dead {
        return None;
    }
    let provider = Provider::from_app(app)?;
    if credential
        .expires_at
        .is_some_and(|expires_at| expires_at <= centaur_console_db::timestamp::now())
    {
        return None;
    }
    let encryption = OauthEncryption::from_env()?;
    let access_token = credential
        .access_token
        .as_deref()
        .map(|ciphertext| encryption.decrypt(ciphertext))
        .transpose()
        .ok()
        .flatten()
        .filter(|token| !token.is_empty())?;
    let subject = credential.provider_subject.clone().unwrap_or_default();
    Some(
        provider
            .details_for(&OauthAppCredential {
                access_token,
                provider_subject: subject,
            })
            .await,
    )
}

fn input_error(preset: Preset) -> ConsoleError {
    ConsoleError::unprocessable(format!("Enter a valid {} MCP URL.", preset.display_name()))
}

fn preset_conflict() -> ConsoleError {
    ConsoleError::new(
        StatusCode::CONFLICT,
        "The preset conflicts with an existing OAuth app. Review its configuration.",
    )
}

fn preset_failed() -> ConsoleError {
    ConsoleError::new(
        StatusCode::BAD_GATEWAY,
        "Could not set up the integration. Please try again or review the OAuth app configuration.",
    )
}

pub(super) async fn console_preset(
    State(state): State<AppState>,
    Path(provider_key): Path<String>,
    user: ConsoleUser,
    body: Result<ConsoleBody<PresetBody>, ConsoleError>,
) -> Result<Response, ConsoleError> {
    let Some(preset) = Preset::fetch(&provider_key) else {
        return Err(ConsoleError::not_found("no preset for this provider"));
    };
    let input = match body {
        Ok(ConsoleBody(body)) => body.data.unwrap_or_default(),
        Err(_) => return Err(input_error(preset)),
    };
    let attributes = preset.attributes();
    let pool = pool(&state)?;
    // An existing app is resolved before a standalone preset's inputs are
    // validated, so a conflicting app wins over an invalid tenant URL.
    let existing = find_oauth_app_by_slug(&pool, &attributes.slug).await?;
    let app = match existing {
        Some(app) => {
            if !preset_matches(&app, &attributes) {
                return Err(preset_conflict());
            }
            app
        }
        None => {
            let Some(setup) = preset.setup(input.resource_url.as_deref()) else {
                return Err(input_error(preset));
            };
            let request = RegistrationRequest {
                endpoint: setup.registration_endpoint.clone(),
                redirect_uri: oauth_callback_redirect_uri(console_config(&state), &attributes.slug),
                scope: registration_scope(&attributes.allowed_scopes),
                auth_method: preset.token_endpoint_auth_method(),
            };
            let (client_id, client_secret) = match providers::register_client(&request).await {
                Ok(registration) => registration,
                Err(_) => return Err(preset_failed()),
            };
            let encryption = OauthEncryption::from_env().ok_or_else(config_failure)?;
            let secret = encryption
                .encrypt(&client_secret)
                .map_err(|_| config_failure())?;
            let new = NewOauthApp {
                slug: attributes.slug.clone(),
                description: Some(attributes.description.clone()),
                labels: attributes.labels.clone(),
                provider: attributes.provider.to_owned(),
                client_id,
                client_secret: secret,
                allowed_scopes: attributes.allowed_scopes.clone(),
                provider_config: setup.provider_config.clone(),
                enabled: true,
                always_available: attributes.always_available,
                shared: attributes.shared,
                owner_only: attributes.owner_only,
                created_by_id: user.id,
            };
            match insert_oauth_app(&pool, &new).await {
                Ok(id) => model::find_by_id::<OauthAppModel, OauthApp, _>(&pool, id)
                    .await?
                    .ok_or_else(preset_failed)?,
                Err(error) if error.is_unique_violation() => {
                    match find_oauth_app_by_slug(&pool, &attributes.slug).await? {
                        Some(app) if preset_matches(&app, &attributes) => app,
                        _ => return Err(preset_conflict()),
                    }
                }
                Err(_) => return Err(preset_failed()),
            }
        }
    };
    Ok(document(PresetPayload {
        id: OauthAppModel::oid(app.id),
        slug: app.slug.clone(),
        start_url: oauth_start_url(console_config(&state), &app.slug),
    })
    .into_response())
}

fn preset_matches(app: &OauthApp, attributes: &providers::PresetAttributes) -> bool {
    app.provider == attributes.provider
        && app.shared == attributes.shared
        && app.owner_only == attributes.owner_only
        && app.enabled
}

pub(super) async fn sandbox_index(State(state): State<AppState>) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let apps = enabled_oauth_apps(&pool, false).await?;
    let data: Vec<SandboxAppPayload> = apps
        .iter()
        .map(|app| SandboxAppPayload {
            id: OauthAppModel::oid(app.id),
            slug: app.slug.clone(),
            description: app.description.clone(),
            labels: app.labels.clone(),
            provider: app.provider.clone(),
            allowed_scopes: app.allowed_scopes.clone(),
            owner_only: app.owner_only,
            resource: resource_of(app),
            start_url: oauth_start_url(console_config(&state), &app.slug),
        })
        .collect();
    Ok(document(data).into_response())
}

fn resource_of(app: &OauthApp) -> Option<String> {
    match app.provider_config.get("resource") {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

pub(super) async fn console_integrations(
    State(state): State<AppState>,
    user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let apps = enabled_oauth_apps(&pool, user.admin).await?;
    let app_ids: Vec<i64> = apps.iter().map(|app| app.id).collect();
    let shared_ids: Vec<i64> = apps
        .iter()
        .filter(|app| app.shared)
        .map(|app| app.id)
        .collect();
    let credentials =
        connections_for_user(&pool, user.id, &user.email, &app_ids, &shared_ids).await?;
    // `index_by` keeps the last row for a repeated app id, and the query is
    // oldest first, so the newest credential wins.
    let mut by_app: HashMap<i64, &BrokerCredential> = HashMap::new();
    for credential in &credentials {
        if let Some(app_id) = credential.oauth_app_id {
            by_app.insert(app_id, credential);
        }
    }
    let app_payloads: Vec<IntegrationAppPayload> = apps
        .iter()
        .map(|app| {
            integration_app_payload(
                app,
                by_app.get(&app.id).copied(),
                &user,
                console_config(&state),
            )
        })
        .collect();
    let presets = if user.admin {
        available_presets(&pool).await
    } else {
        Vec::new()
    };
    let ssh_key = user_has_ssh_key(&pool, user.id).await?;
    Ok(document(IntegrationsPayload {
        apps: app_payloads,
        presets,
        ssh_key: SshKeyPayload {
            configured: ssh_key,
        },
    })
    .into_response())
}

fn usage_of(app: &OauthApp) -> &'static str {
    if app.shared {
        "shared"
    } else if app.always_available {
        "always_available"
    } else {
        "direct_messages"
    }
}

fn credential_needs_reconnect(app: &OauthApp, credential: &BrokerCredential) -> bool {
    if credential.dead {
        return true;
    }
    let required = Provider::from_app(app)
        .map(|provider| provider.required_scopes())
        .unwrap_or_default();
    required
        .iter()
        .any(|scope| !credential.scopes.iter().any(|granted| granted == scope))
}

fn integration_app_payload(
    app: &OauthApp,
    credential: Option<&BrokerCredential>,
    user: &ConsoleUser,
    config: &ConsoleSessionConfig,
) -> IntegrationAppPayload {
    IntegrationAppPayload {
        id: OauthAppModel::oid(app.id),
        slug: app.slug.clone(),
        provider: app.provider.clone(),
        description: app.description.clone(),
        shared: app.shared,
        always_available: app.always_available,
        owner_only: app.owner_only,
        usage: usage_of(app),
        start_url: oauth_start_url(config, &app.slug),
        connection: credential.map(|credential| ConnectionPayload {
            id: BrokerCredentialModel::oid(credential.id),
            status: if credential_needs_reconnect(app, credential) {
                "needs_reconnect"
            } else {
                "connected"
            },
            can_disconnect: !app.shared
                && credential.created_by_id == Some(user.id)
                && !credential.dead,
        }),
    }
}

async fn available_presets(pool: &PgPool) -> Vec<IntegrationPresetPayload> {
    let slugs = all_oauth_app_slugs(pool).await.unwrap_or_default();
    let provider_keys = all_oauth_app_providers(pool).await.unwrap_or_default();
    Preset::all()
        .into_iter()
        .filter(|preset| {
            let attributes = preset.attributes();
            !slugs.iter().any(|slug| slug == &attributes.slug)
                && !(*preset == Preset::Mercator
                    && provider_keys
                        .iter()
                        .any(|provider| provider == providers::MERCATOR_KEY))
        })
        .map(|preset| {
            let attributes = preset.attributes();
            IntegrationPresetPayload {
                provider: preset.key(),
                display_name: preset.display_name(),
                description: attributes.description,
                inputs: preset.setup_inputs(),
            }
        })
        .collect()
}

pub(super) async fn console_disconnect(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let credential = credential_by_oid(&pool, &id).await?;
    let Some(app_id) = credential.oauth_app_id else {
        return Err(BrokerCredentialModel::not_found().into());
    };
    let app = model::find_by_id::<OauthAppModel, OauthApp, _>(&pool, app_id)
        .await?
        .ok_or_else(BrokerCredentialModel::not_found)?;
    if credential.created_by_id != Some(user.id) || app.shared {
        return Err(BrokerCredentialModel::not_found().into());
    }
    let encryption = OauthEncryption::from_env().ok_or_else(config_failure)?;
    let tokens = disconnect_broker_credential(&pool, credential.id, &encryption)
        .await?
        .ok_or_else(BrokerCredentialModel::not_found)?;
    // Reconciliation for the updated credential.
    let mut connection = pool.acquire().await?;
    centaur_console_db::areas::reconciliation::apply_for_credential(&mut connection, credential.id)
        .await?;
    let provider = Provider::from_app(&app);
    let endpoint = provider.as_ref().and_then(Provider::revocation_endpoint);
    let outcome = match endpoint {
        None => "local_only",
        Some(endpoint) => {
            let client_secret = app
                .client_secret
                .as_deref()
                .map(|secret| encryption.decrypt(secret))
                .transpose()
                .ok()
                .flatten();
            let mut requests = Vec::new();
            for (token, hint) in [
                (tokens.refresh_token.as_deref(), "refresh_token"),
                (tokens.access_token.as_deref(), "access_token"),
            ] {
                let Some(token) = token.filter(|token| !token.is_empty()) else {
                    continue;
                };
                let mut form = vec![
                    ("client_id".to_owned(), app.client_id.clone()),
                    ("token".to_owned(), token.to_owned()),
                    ("token_type_hint".to_owned(), hint.to_owned()),
                ];
                if let Some(secret) = &client_secret {
                    form.insert(1, ("client_secret".to_owned(), secret.clone()));
                }
                requests.push(
                    super::http::send(
                        reqwest::Method::POST,
                        &endpoint,
                        &[],
                        Some(&form),
                        None,
                        std::time::Duration::from_secs(5),
                        64 * 1024,
                    )
                    .await
                    .is_ok_and(|response| response.success()),
                );
            }
            if requests.iter().all(|success| *success) {
                "revoked"
            } else {
                "failed"
            }
        }
    };
    Ok(document(DisconnectPayload {
        id: BrokerCredentialModel::oid(credential.id),
        outcome,
    })
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_write() -> AppWrite {
        let mut write = AppWrite::new(1, None);
        write.slug = "example".to_owned();
        write.provider = "google".to_owned();
        write.client_id = "client".to_owned();
        write.client_secret_stored = Some("secret".to_owned());
        write.allowed_scopes = vec!["scope.read".to_owned()];
        write
    }

    use super::super::error_keys as keys;

    #[test]
    fn app_validation_accepts_a_valid_app() {
        assert!(app_validation(&app_write(), None, None, false).is_empty());
    }

    #[test]
    fn app_validation_rejects_invalid_fields() {
        let mut config = Map::new();
        config.insert(
            "token_endpoint".to_owned(),
            Value::String("https://auth.example.test/token".to_owned()),
        );
        let mut write = app_write();
        write.slug = "oap_x/y".to_owned();
        write.provider = "nope".to_owned();
        write.client_id = String::new();
        write.client_secret_stored = None;
        write.allowed_scopes = Vec::new();
        write.provider_config = config;

        let mut keys = keys(app_validation(&write, None, None, true));
        keys.sort();
        assert_eq!(
            keys,
            [
                "allowed_scopes",
                "client_id",
                "client_secret",
                "provider",
                "provider_config",
                "slug",
            ]
        );
    }

    #[test]
    fn app_validation_rejects_cross_field_changes() {
        let mut write = app_write();
        write.shared = true;
        write.shared_touched = Some(true);
        write.always_available = true;
        write.owner_only = true;
        write.owner_only_touched = Some(true);

        let mut keys = keys(app_validation(&write, Some(false), Some(false), false));
        keys.sort();
        assert_eq!(keys, ["owner_only", "shared"]);
    }
}
