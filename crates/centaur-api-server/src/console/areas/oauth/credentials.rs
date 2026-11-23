//! Broker credential CRUD handlers.
//!
//! Oid/foreign_id addressing, label search, PUT upsert, write-only initial
//! values, and the console's presence-only payload fields. The rotating token
//! blob is never serialized back.

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
        BrokerCredential, BrokerCredentialWrite, DEFAULT_EARLY_REFRESH_FRACTION,
        DEFAULT_EARLY_REFRESH_SLACK_SECONDS, DEFAULT_MAX_REFRESH_INTERVAL_SECONDS, OauthEncryption,
        PREQIN_TOKEN_ENDPOINT, URL_SAFE_MESSAGE, credential_referenced_by_secret_source,
        delete_broker_credential, insert_broker_credential, oauth_app_for_credential,
        update_broker_credential, url_safe, wrapping_secret_for_credential,
    },
    model::{
        self, BrokerCredential as BrokerCredentialModel, ConsoleModel, OauthApp as OauthAppModel,
        StaticSecret as StaticSecretModel,
    },
    timestamp,
    validation::Validation,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use time::PrimitiveDateTime;

use super::{
    Patch, blank, config_failure, created, credential_by_oid, document, is_oid, label_filter, pool,
    present,
};
use crate::{
    AppState,
    console::{
        guard::BearerUser,
        http::{ConsoleError, ListParams, ListSource, bind, list, list_response, list_rows},
        request::ConsoleData,
    },
};

const GRANTS: [&str; 4] = ["refresh_token", "client_credentials", "password", "preqin"];

/// The credential list: the label filter binds `$1`.
const CREDENTIAL_LIST: ListSource = ListSource::new(
    centaur_console_db::areas::oauth::CREDENTIAL_LIST_SELECT,
    centaur_console_db::areas::oauth::CREDENTIAL_LIST_FROM,
    centaur_console_db::areas::oauth::CREDENTIAL_LIST_ORDER,
    1,
);

/// One broker credential document write. Unknown fields are ignored; a field
/// of the wrong JSON type is a validation error, not a cast.
#[derive(Debug, Default, Deserialize)]
pub(super) struct BrokerCredentialInput {
    #[serde(default)]
    foreign_id: Patch<String>,
    #[serde(default)]
    name: Patch<String>,
    #[serde(default)]
    description: Patch<String>,
    #[serde(default)]
    token_endpoint: Patch<String>,
    #[serde(default)]
    grant: Patch<String>,
    #[serde(default)]
    client_id: Patch<String>,
    #[serde(default)]
    labels: Option<Map<String, Value>>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    early_refresh_fraction: Patch<f64>,
    #[serde(default)]
    early_refresh_slack_seconds: Patch<i64>,
    #[serde(default)]
    max_refresh_interval_seconds: Patch<i64>,
    #[serde(default)]
    refresh_timeout_seconds: Patch<i64>,
    #[serde(default)]
    client_secret: Patch<String>,
    #[serde(default)]
    refresh_token: Patch<String>,
    #[serde(default)]
    username: Patch<String>,
    #[serde(default)]
    password: Patch<String>,
    #[serde(default)]
    api_key: Patch<String>,
    #[serde(default)]
    token_endpoint_headers: Patch<EndpointHeaders>,
}

/// `token_endpoint_headers` is an open map whose values are later validated as
/// strings; a non-object stays an `Invalid` value so the model's message wins
/// over a generic type error.
#[derive(Clone, Debug)]
pub(super) enum EndpointHeaders {
    Object(Map<String, Value>),
    Invalid,
}

impl<'de> Deserialize<'de> for EndpointHeaders {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Value::deserialize(deserializer)? {
            Value::Object(map) => Self::Object(map),
            _ => Self::Invalid,
        })
    }
}

fn apply_optional_string(target: &mut Option<String>, patch: &Patch<String>) {
    match patch {
        Patch::Value(value) => *target = Some(value.clone()),
        Patch::Null => *target = None,
        Patch::Missing => {}
    }
}

fn apply_string(target: &mut String, patch: &Patch<String>) {
    match patch {
        Patch::Value(value) => *target = value.clone(),
        Patch::Null => target.clear(),
        Patch::Missing => {}
    }
}

fn header_names(credential: &BrokerCredential) -> Vec<String> {
    let Some(encryption) = OauthEncryption::from_env() else {
        return Vec::new();
    };
    credential
        .token_endpoint_header_names(&encryption)
        .unwrap_or_default()
}

/// The `data` object of a broker credential response.
#[derive(Serialize)]
pub(super) struct CredentialPayload {
    id: String,
    foreign_id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    labels: Map<String, Value>,
    grant: String,
    token_endpoint: String,
    scopes: Vec<String>,
    client_id: Option<String>,
    token_endpoint_header_names: Vec<String>,
    early_refresh_slack_seconds: i32,
    early_refresh_fraction: f64,
    max_refresh_interval_seconds: i32,
    refresh_timeout_seconds: i32,
    status: &'static str,
    #[serde(with = "timestamp::primitive_option")]
    expires_at: Option<PrimitiveDateTime>,
    #[serde(with = "timestamp::primitive_option")]
    last_refresh: Option<PrimitiveDateTime>,
    #[serde(with = "timestamp::primitive_option")]
    next_attempt_at: Option<PrimitiveDateTime>,
    dead: bool,
    dead_reason: Option<String>,
    failure_count: i32,
    oauth_app_id: Option<String>,
    provider_subject: Option<String>,
    provider_email: Option<String>,
    external_user_key: Option<String>,
    #[serde(with = "timestamp::primitive")]
    created_at: PrimitiveDateTime,
    #[serde(with = "timestamp::primitive")]
    updated_at: PrimitiveDateTime,
}

impl From<&BrokerCredential> for CredentialPayload {
    fn from(credential: &BrokerCredential) -> Self {
        Self {
            id: BrokerCredentialModel::oid(credential.id),
            foreign_id: credential.foreign_id.clone(),
            name: credential.name.clone(),
            description: credential.description.clone(),
            labels: credential.labels.clone(),
            grant: credential.grant.clone(),
            token_endpoint: credential.token_endpoint.clone(),
            scopes: credential.scopes.clone(),
            client_id: credential.client_id.clone(),
            token_endpoint_header_names: header_names(credential),
            early_refresh_slack_seconds: credential.early_refresh_slack_seconds,
            early_refresh_fraction: credential.early_refresh_fraction,
            max_refresh_interval_seconds: credential.max_refresh_interval_seconds,
            refresh_timeout_seconds: credential.refresh_timeout_seconds,
            status: credential.status(),
            expires_at: credential.expires_at,
            last_refresh: credential.last_refresh,
            next_attempt_at: credential.next_attempt_at,
            dead: credential.dead,
            dead_reason: credential.dead_reason.clone(),
            failure_count: credential.failure_count,
            oauth_app_id: credential.oauth_app_id.map(OauthAppModel::oid),
            provider_subject: credential.provider_subject.clone(),
            provider_email: credential.provider_email.clone(),
            external_user_key: credential.external_user_key.clone(),
            created_at: credential.created_at,
            updated_at: credential.updated_at,
        }
    }
}

fn payload(credential: &BrokerCredential) -> CredentialPayload {
    CredentialPayload::from(credential)
}

/// The console payload adds presence-only write-only fields, the effective
/// client id, and the linked app/secret summaries.
#[derive(Serialize)]
struct ConsoleCredentialPayload {
    #[serde(flatten)]
    credential: CredentialPayload,
    has_client_secret: bool,
    has_refresh_token: bool,
    has_username: bool,
    has_password: bool,
    has_api_key: bool,
    effective_client_id: Option<String>,
    oauth_app: Option<CredentialAppPayload>,
    static_secret: Option<WrappingSecretPayload>,
}

#[derive(Serialize)]
struct CredentialAppPayload {
    id: String,
    slug: String,
    provider: String,
}

#[derive(Serialize)]
struct WrappingSecretPayload {
    id: String,
    foreign_id: Option<String>,
    name: Option<String>,
}

async fn console_credential_payload(
    pool: &sqlx::PgPool,
    credential: &BrokerCredential,
) -> Result<ConsoleCredentialPayload, ConsoleError> {
    let app = match credential.oauth_app_id {
        Some(app_id) => oauth_app_for_credential(pool, app_id).await?,
        None => None,
    };
    let secret = wrapping_secret_for_credential(pool, credential.id).await?;
    Ok(ConsoleCredentialPayload {
        has_client_secret: credential
            .client_secret
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        has_refresh_token: credential
            .refresh_token
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        has_username: credential
            .username
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        has_password: credential
            .password
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        has_api_key: credential
            .api_key
            .as_deref()
            .is_some_and(|value| !value.is_empty()),
        effective_client_id: app
            .as_ref()
            .map(|app| app.client_id.clone())
            .or_else(|| credential.client_id.clone()),
        oauth_app: app.as_ref().map(|app| CredentialAppPayload {
            id: OauthAppModel::oid(app.id),
            slug: app.slug.clone(),
            provider: app.provider.clone(),
        }),
        static_secret: secret.as_ref().map(|secret| WrappingSecretPayload {
            id: StaticSecretModel::oid(secret.id),
            foreign_id: secret.foreign_id.clone(),
            name: secret.name.clone(),
        }),
        credential: CredentialPayload::from(credential),
    })
}

/// The complete replace-style write, built from the existing row plus body. The
/// per-field rules are garde attributes here because this merged record is what
/// the model validates; rules that need the stored row or the grant's secret
/// material stay in [`Write::validation_errors`].
#[derive(garde::Validate)]
struct Write {
    #[garde(skip)]
    id: Option<i64>,
    #[garde(custom(foreign_id_url_safe))]
    foreign_id: Option<String>,
    #[garde(skip)]
    name: Option<String>,
    #[garde(skip)]
    description: Option<String>,
    #[garde(skip)]
    labels: Map<String, Value>,
    #[garde(custom(valid_grant))]
    grant: String,
    #[garde(custom(present))]
    token_endpoint: String,
    #[garde(skip)]
    scopes: Vec<String>,
    #[garde(skip)]
    client_id: Option<String>,
    #[garde(custom(early_refresh_fraction_range))]
    early_refresh_fraction: f64,
    #[garde(range(min = 1))]
    early_refresh_slack_seconds: i32,
    #[garde(range(min = 1))]
    max_refresh_interval_seconds: i32,
    #[garde(range(min = 1))]
    refresh_timeout_seconds: i32,
    #[garde(skip)]
    stored: StoredSecrets,
    #[garde(skip)]
    incoming: IncomingSecrets,
    #[garde(skip)]
    reset_refresh_state: bool,
    #[garde(skip)]
    foreign_id_in_database: Option<String>,
}

#[derive(Default)]
struct StoredSecrets {
    client_secret: Option<String>,
    refresh_token: Option<String>,
    username: Option<String>,
    password: Option<String>,
    api_key: Option<String>,
    token_endpoint_headers: Option<String>,
}

#[derive(Default)]
struct IncomingSecrets {
    client_secret: Option<String>,
    refresh_token: Option<String>,
    username: Option<String>,
    password: Option<String>,
    api_key: Option<String>,
    headers: Patch<EndpointHeaders>,
}

impl Write {
    fn defaults(identity_foreign_id: Option<String>) -> Self {
        Self {
            id: None,
            foreign_id: identity_foreign_id,
            name: None,
            description: None,
            labels: Map::new(),
            grant: "refresh_token".to_owned(),
            token_endpoint: String::new(),
            scopes: Vec::new(),
            client_id: None,
            early_refresh_fraction: DEFAULT_EARLY_REFRESH_FRACTION,
            early_refresh_slack_seconds: DEFAULT_EARLY_REFRESH_SLACK_SECONDS,
            max_refresh_interval_seconds: DEFAULT_MAX_REFRESH_INTERVAL_SECONDS,
            refresh_timeout_seconds: 30,
            stored: StoredSecrets::default(),
            incoming: IncomingSecrets::default(),
            reset_refresh_state: false,
            foreign_id_in_database: None,
        }
    }

    fn from_existing(credential: &BrokerCredential) -> Self {
        Self {
            id: Some(credential.id),
            foreign_id: credential.foreign_id.clone(),
            name: credential.name.clone(),
            description: credential.description.clone(),
            labels: credential.labels.clone(),
            grant: credential.grant.clone(),
            token_endpoint: credential.token_endpoint.clone(),
            scopes: credential.scopes.clone(),
            client_id: credential.client_id.clone(),
            early_refresh_fraction: credential.early_refresh_fraction,
            early_refresh_slack_seconds: credential.early_refresh_slack_seconds,
            max_refresh_interval_seconds: credential.max_refresh_interval_seconds,
            refresh_timeout_seconds: credential.refresh_timeout_seconds,
            stored: StoredSecrets {
                client_secret: credential.client_secret.clone(),
                refresh_token: credential.refresh_token.clone(),
                username: credential.username.clone(),
                password: credential.password.clone(),
                api_key: credential.api_key.clone(),
                token_endpoint_headers: credential.token_endpoint_headers.clone(),
            },
            incoming: IncomingSecrets::default(),
            reset_refresh_state: false,
            foreign_id_in_database: credential.foreign_id.clone(),
        }
    }

    fn apply(&mut self, data: &BrokerCredentialInput) {
        // A PUT upsert by foreign_id sets identity before assignment; a blank
        // body value must not wipe it.
        if !matches!(data.foreign_id, Patch::Missing) {
            let value = data.foreign_id.clone().into_option();
            let current = self.foreign_id.clone();
            let keep_current = value.as_deref().is_none_or(blank)
                && current.as_deref().is_some_and(|value| !blank(value));
            if !keep_current {
                self.foreign_id = value;
            }
        }

        apply_optional_string(&mut self.name, &data.name);
        apply_optional_string(&mut self.description, &data.description);
        apply_optional_string(&mut self.client_id, &data.client_id);
        apply_string(&mut self.grant, &data.grant);
        apply_string(&mut self.token_endpoint, &data.token_endpoint);

        if let Some(labels) = &data.labels {
            self.labels = labels.clone();
        }
        if let Some(scopes) = &data.scopes {
            self.scopes = scopes.clone();
        }
        if let Patch::Value(value) = &data.early_refresh_fraction {
            self.early_refresh_fraction = *value;
        }
        if let Patch::Value(value) = &data.early_refresh_slack_seconds {
            self.early_refresh_slack_seconds = *value as i32;
        }
        if let Patch::Value(value) = &data.max_refresh_interval_seconds {
            self.max_refresh_interval_seconds = *value as i32;
        }
        if let Patch::Value(value) = &data.refresh_timeout_seconds {
            self.refresh_timeout_seconds = *value as i32;
        }

        // Write-only initial values: a non-blank value replaces the stored
        // material and puts the credential back in the "due now" state.
        let mut changed = false;
        if let Patch::Value(secret) = &data.client_secret
            && !blank(secret)
        {
            self.incoming.client_secret = Some(secret.clone());
            if self.grant == "client_credentials" {
                self.reset_refresh_state = true;
            }
        }
        for (target, patch) in [
            (&mut self.incoming.refresh_token, &data.refresh_token),
            (&mut self.incoming.username, &data.username),
            (&mut self.incoming.password, &data.password),
            (&mut self.incoming.api_key, &data.api_key),
        ] {
            if let Patch::Value(value) = patch
                && !blank(value)
            {
                *target = Some(value.clone());
                changed = true;
            }
        }
        if changed {
            self.reset_refresh_state = true;
        }

        match &data.token_endpoint_headers {
            Patch::Missing => {}
            Patch::Null => self.incoming.headers = Patch::Null,
            Patch::Value(value) => self.incoming.headers = Patch::Value(value.clone()),
        }
    }

    fn preqin_default(&mut self) {
        if self.grant == "preqin" {
            self.token_endpoint = PREQIN_TOKEN_ENDPOINT.to_owned();
        }
    }

    fn effective_headers(&self, console: bool) -> Option<HeadersValue> {
        match &self.incoming.headers {
            Patch::Missing => Some(HeadersValue::Stored(
                self.stored.token_endpoint_headers.clone(),
            )),
            Patch::Null => Some(HeadersValue::Clear),
            Patch::Value(EndpointHeaders::Object(object)) => {
                let mut headers = object.clone();
                if console
                    && let Some(stored) = self
                        .stored
                        .token_endpoint_headers
                        .as_deref()
                        .and_then(|ciphertext| {
                            OauthEncryption::from_env()
                                .and_then(|encryption| encryption.decrypt(ciphertext).ok())
                        })
                        .and_then(|json| serde_json::from_str::<Value>(&json).ok())
                        .and_then(|value| value.as_object().cloned())
                {
                    for (name, value) in headers.iter_mut() {
                        if value.is_null()
                            && let Some(stored_value) = stored.get(name)
                        {
                            *value = stored_value.clone();
                        }
                    }
                }
                Some(HeadersValue::Object(headers))
            }
            Patch::Value(EndpointHeaders::Invalid) => Some(HeadersValue::Invalid),
        }
    }

    fn effective_client_secret(&self) -> Option<&str> {
        self.incoming
            .client_secret
            .as_deref()
            .filter(|value| !blank(value))
            .or(self.stored.client_secret.as_deref())
    }

    fn validation_errors(&self, headers: &HeadersValue) -> FieldErrors {
        let mut validation = Validation::new();
        validation.check(self);
        if self
            .foreign_id_in_database
            .as_deref()
            .is_some_and(|database| !blank(database))
            && self.foreign_id.as_deref() != self.foreign_id_in_database.as_deref()
        {
            validation.add("foreign_id", "cannot be changed once set");
        }
        if self.grant != "preqin" && self.client_id.as_deref().is_none_or(blank) {
            validation.add("client_id", "can't be blank");
        }
        if self.grant == "client_credentials" && self.effective_client_secret().is_none_or(blank) {
            validation.add("client_secret", "can't be blank");
        }
        let username_message = match self.grant.as_str() {
            "password" => Some("can't be blank for the password grant"),
            "preqin" => Some("can't be blank for the Preqin broker grant"),
            _ => None,
        };
        if let Some(message) = username_message
            && secret_missing(&self.incoming.username, &self.stored.username)
        {
            validation.add("username", message);
        }
        if self.grant == "password"
            && secret_missing(&self.incoming.password, &self.stored.password)
        {
            validation.add("password", "can't be blank for the password grant");
        }
        if self.grant == "preqin" && secret_missing(&self.incoming.api_key, &self.stored.api_key) {
            validation.add("api_key", "can't be blank for the Preqin broker grant");
        }
        if headers_invalid(headers) {
            validation.add("token_endpoint_headers", HEADERS_MESSAGE);
        }
        validation.into_result().err().unwrap_or_default()
    }
}

const HEADERS_MESSAGE: &str = "must be an object mapping header names to string values";

fn secret_missing(incoming: &Option<String>, stored: &Option<String>) -> bool {
    incoming.as_deref().or(stored.as_deref()).is_none_or(blank)
}

fn valid_grant(value: &str, _: &()) -> garde::Result {
    if !GRANTS.contains(&value) {
        return Err(garde::Error::new(format!(
            "must be one of {}",
            GRANTS.join(", ")
        )));
    }
    Ok(())
}

fn foreign_id_url_safe(value: &Option<String>, _: &()) -> garde::Result {
    if value.as_deref().is_some_and(|value| !url_safe(value)) {
        return Err(garde::Error::new(URL_SAFE_MESSAGE));
    }
    Ok(())
}

fn early_refresh_fraction_range(value: &f64, _: &()) -> garde::Result {
    if *value < 0.0 {
        return Err(garde::Error::new("must be greater than or equal to 0"));
    }
    if *value >= 1.0 {
        return Err(garde::Error::new("must be less than 1"));
    }
    Ok(())
}

fn headers_invalid(value: &HeadersValue) -> bool {
    match value {
        HeadersValue::Object(headers) => headers.values().any(|value| !value.is_string()),
        HeadersValue::Invalid => true,
        HeadersValue::Stored(_) | HeadersValue::Clear => false,
    }
}

#[derive(Clone, Debug)]
enum HeadersValue {
    Stored(Option<String>),
    Clear,
    Object(Map<String, Value>),
    Invalid,
}

fn encrypt_secret(
    encryption: &OauthEncryption,
    value: Option<&str>,
) -> Result<Option<String>, ConsoleError> {
    value
        .map(|value| encryption.encrypt(value).map_err(ConsoleError::from))
        .transpose()
}

fn write_values(
    write: &Write,
    headers: &HeadersValue,
) -> Result<(StoredSecrets, Option<String>), ConsoleError> {
    let encryption = OauthEncryption::from_env().ok_or_else(config_failure)?;
    let stored = StoredSecrets {
        client_secret: write
            .incoming
            .client_secret
            .as_deref()
            .map(|value| encryption.encrypt(value))
            .transpose()
            .map_err(ConsoleError::from)?
            .or_else(|| write.stored.client_secret.clone()),
        refresh_token: encrypt_secret(&encryption, write.incoming.refresh_token.as_deref())?
            .or_else(|| write.stored.refresh_token.clone()),
        username: encrypt_secret(&encryption, write.incoming.username.as_deref())?
            .or_else(|| write.stored.username.clone()),
        password: encrypt_secret(&encryption, write.incoming.password.as_deref())?
            .or_else(|| write.stored.password.clone()),
        api_key: encrypt_secret(&encryption, write.incoming.api_key.as_deref())?
            .or_else(|| write.stored.api_key.clone()),
        token_endpoint_headers: match headers {
            HeadersValue::Stored(value) => value.clone(),
            HeadersValue::Clear | HeadersValue::Invalid => None,
            HeadersValue::Object(headers) => {
                let json = serde_json::to_string(&Value::Object(headers.clone()))
                    .map_err(ConsoleError::internal)?;
                Some(encryption.encrypt(&json).map_err(ConsoleError::from)?)
            }
        },
    };
    let headers_ciphertext = stored.token_endpoint_headers.clone();
    Ok((stored, headers_ciphertext))
}

async fn save_credential(
    pool: &sqlx::PgPool,
    data: &BrokerCredentialInput,
    existing: Option<BrokerCredential>,
    identity_foreign_id: Option<String>,
    created_by_id: i64,
    console: bool,
) -> Result<BrokerCredential, ConsoleError> {
    let linked = existing
        .as_ref()
        .is_some_and(|credential| credential.oauth_app_id.is_some());
    let mut write = match &existing {
        Some(credential) => Write::from_existing(credential),
        None => Write::defaults(identity_foreign_id),
    };
    write.apply(data);
    write.preqin_default();
    let headers = write
        .effective_headers(console)
        .unwrap_or(HeadersValue::Clear);
    let errors = write.validation_errors(&headers);
    if !errors.is_empty() {
        return Err(errors.into());
    }
    if let Some(foreign_id) = write.foreign_id.as_deref()
        && !blank(foreign_id)
    {
        let other = model::find_by_foreign_id::<BrokerCredentialModel, BrokerCredential, _>(
            pool, foreign_id,
        )
        .await?;
        if let Some(other) = other
            && write.id.is_none_or(|id| id != other.id)
        {
            let mut errors = FieldErrors::new();
            errors.add("foreign_id", "has already been taken");
            return Err(errors.into());
        }
    }
    let (stored, headers_ciphertext) = write_values(&write, &headers)?;
    let row = BrokerCredentialWrite {
        id: write.id,
        foreign_id: write.foreign_id.clone(),
        name: write.name.clone(),
        description: write.description.clone(),
        labels: write.labels.clone(),
        grant: write.grant.clone(),
        token_endpoint: write.token_endpoint.clone(),
        scopes: write.scopes.clone(),
        client_id: write.client_id.clone(),
        client_secret: stored.client_secret.clone(),
        username: stored.username.clone(),
        password: stored.password.clone(),
        api_key: stored.api_key.clone(),
        token_endpoint_headers: headers_ciphertext.clone(),
        refresh_token: stored.refresh_token.clone(),
        early_refresh_fraction: write.early_refresh_fraction,
        early_refresh_slack_seconds: write.early_refresh_slack_seconds,
        max_refresh_interval_seconds: write.max_refresh_interval_seconds,
        refresh_timeout_seconds: write.refresh_timeout_seconds,
        reset_refresh_state: write.reset_refresh_state,
        created_by_id: Some(created_by_id),
    };
    let id = match row.id {
        Some(id) => {
            update_broker_credential(pool, id, &row).await?;
            id
        }
        None => insert_broker_credential(pool, &row).await?,
    };
    let credential = model::find_by_id::<BrokerCredentialModel, BrokerCredential, _>(pool, id)
        .await?
        .ok_or_else(BrokerCredentialModel::not_found)?;
    if linked {
        // Reconciliation for a flow-minted credential.
        let mut connection = pool.acquire().await?;
        centaur_console_db::areas::reconciliation::apply_for_credential(&mut connection, id)
            .await?;
    }
    Ok(credential)
}

pub(super) async fn console_index(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    let params = ListParams::from_query(&query)?;
    let pool = pool(&state)?;
    let (credentials, total) =
        list_rows::<BrokerCredential, _>(&pool, &params, CREDENTIAL_LIST, |arguments| {
            bind(arguments, label_filter(&params))
        })
        .await?;
    let mut data = Vec::with_capacity(credentials.len());
    for credential in &credentials {
        data.push(console_credential_payload(&pool, credential).await?);
    }
    Ok(Json(list_response(params.pagination, &data, total)).into_response())
}

pub(super) async fn bearer_index(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    let params = ListParams::from_query(&query)?;
    let pool = pool(&state)?;
    let body = list::<BrokerCredential, CredentialPayload, _, _>(
        &pool,
        &params,
        CREDENTIAL_LIST,
        |arguments| bind(arguments, label_filter(&params)),
        payload,
    )
    .await?;
    Ok(Json(body).into_response())
}

async fn show(pool: &sqlx::PgPool, id: &str, console: bool) -> Result<Response, ConsoleError> {
    let credential = credential_by_oid(pool, id).await?;
    if console {
        return Ok(document(console_credential_payload(pool, &credential).await?).into_response());
    }
    Ok(document(CredentialPayload::from(&credential)).into_response())
}

pub(super) async fn console_show(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    show(&pool, &id, true).await
}

pub(super) async fn bearer_show(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    show(&pool, &id, false).await
}

pub(super) async fn bearer_lookup(
    State(state): State<AppState>,
    Path(foreign_id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let credential =
        model::find_by_foreign_id::<BrokerCredentialModel, BrokerCredential, _>(&pool, &foreign_id)
            .await?
            .ok_or_else(BrokerCredentialModel::not_found)?;
    Ok(document(CredentialPayload::from(&credential)).into_response())
}

pub(super) async fn console_create(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleData(data): ConsoleData<BrokerCredentialInput>,
) -> Result<Response, ConsoleError> {
    create(&state, user.id, &data, true).await
}

pub(super) async fn bearer_create(
    State(state): State<AppState>,
    user: BearerUser,
    ConsoleData(data): ConsoleData<BrokerCredentialInput>,
) -> Result<Response, ConsoleError> {
    create(&state, user.0.id, &data, false).await
}

async fn create(
    state: &AppState,
    user_id: i64,
    data: &BrokerCredentialInput,
    console: bool,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let credential = save_credential(&pool, data, None, None, user_id, console).await?;
    if console {
        return Ok(created(console_credential_payload(&pool, &credential).await?).into_response());
    }
    Ok(created(CredentialPayload::from(&credential)).into_response())
}

pub(super) async fn console_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
    ConsoleData(data): ConsoleData<BrokerCredentialInput>,
) -> Result<Response, ConsoleError> {
    update(&state, &id, user.id, &data, true).await
}

pub(super) async fn bearer_update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: BearerUser,
    ConsoleData(data): ConsoleData<BrokerCredentialInput>,
) -> Result<Response, ConsoleError> {
    update(&state, &id, user.0.id, &data, false).await
}

async fn update(
    state: &AppState,
    identifier: &str,
    user_id: i64,
    data: &BrokerCredentialInput,
    console: bool,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let (existing, identity) = if is_oid::<BrokerCredentialModel>(identifier) {
        (Some(credential_by_oid(&pool, identifier).await?), None)
    } else {
        let existing = model::find_by_foreign_id::<BrokerCredentialModel, BrokerCredential, _>(
            &pool, identifier,
        )
        .await?;
        (existing, Some(identifier.to_owned()))
    };
    let was_new = existing.is_none();
    let credential = save_credential(&pool, data, existing, identity, user_id, console).await?;
    let status = if was_new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    if console {
        return Ok((
            status,
            document(console_credential_payload(&pool, &credential).await?),
        )
            .into_response());
    }
    Ok((status, document(CredentialPayload::from(&credential))).into_response())
}

pub(super) async fn destroy(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let credential = credential_by_oid(&pool, &id).await?;
    if credential_referenced_by_secret_source(&pool, credential.id).await? {
        return Err(ConsoleError::new(
            StatusCode::CONFLICT,
            "is referenced by one or more token_broker secret sources; remove those references first",
        ));
    }
    delete_broker_credential(&pool, credential.id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_write() -> Write {
        let mut write = Write::defaults(None);
        write.foreign_id = Some("example".to_owned());
        write.token_endpoint = "https://auth.example.test/token".to_owned();
        write.client_id = Some("client".to_owned());
        write
    }

    use super::super::error_keys as keys;

    #[test]
    fn credential_validation_accepts_a_valid_write() {
        assert!(
            base_write()
                .validation_errors(&HeadersValue::Stored(None))
                .is_empty()
        );
    }

    #[test]
    fn credential_validation_rejects_invalid_fields() {
        let mut write = base_write();
        write.grant = "bogus".to_owned();
        write.foreign_id = Some("bad/id".to_owned());
        write.foreign_id_in_database = Some("example".to_owned());
        write.token_endpoint = String::new();
        write.client_id = None;
        write.early_refresh_fraction = 1.5;
        write.early_refresh_slack_seconds = 0;
        write.max_refresh_interval_seconds = 0;
        write.refresh_timeout_seconds = 0;

        assert_eq!(
            keys(write.validation_errors(&HeadersValue::Invalid)),
            [
                "client_id",
                "early_refresh_fraction",
                "early_refresh_slack_seconds",
                "foreign_id",
                "grant",
                "max_refresh_interval_seconds",
                "refresh_timeout_seconds",
                "token_endpoint",
                "token_endpoint_headers",
            ]
        );
    }

    #[test]
    fn grant_credentials_are_required_for_the_grants_that_use_them() {
        let mut write = base_write();
        write.grant = "client_credentials".to_owned();
        assert_eq!(
            keys(write.validation_errors(&HeadersValue::Stored(None))),
            ["client_secret"]
        );
        write.incoming.client_secret = Some("secret".to_owned());
        assert!(
            write
                .validation_errors(&HeadersValue::Stored(None))
                .is_empty()
        );

        let mut write = base_write();
        write.grant = "password".to_owned();
        assert_eq!(
            keys(write.validation_errors(&HeadersValue::Stored(None))),
            ["password", "username"]
        );
        write.incoming.username = Some("user".to_owned());
        write.incoming.password = Some("secret".to_owned());
        assert!(
            write
                .validation_errors(&HeadersValue::Stored(None))
                .is_empty()
        );

        let mut write = base_write();
        write.grant = "preqin".to_owned();
        write.client_id = None;
        assert_eq!(
            keys(write.validation_errors(&HeadersValue::Stored(None))),
            ["api_key", "username"]
        );
        write.incoming.username = Some("user".to_owned());
        write.incoming.api_key = Some("key".to_owned());
        assert!(
            write
                .validation_errors(&HeadersValue::Stored(None))
                .is_empty()
        );
    }
}
