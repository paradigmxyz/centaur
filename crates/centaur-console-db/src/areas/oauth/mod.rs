//! Data access for the OAuth area: Console's `oauth_apps` and
//! `broker_credentials`, plus the wrappers the consent flow creates.
//!
//! This module only maps rows, encrypts/decrypts write-only attributes with the
//! Active Record encryption format existing rows use, and runs the queries the
//! handlers need.
//!
//! The rows derive [`sqlx::FromRow`] and are read with `query_as`, the table
//! descriptors are [`OauthAppModel`]/[`BrokerCredentialModel`], inserts and
//! updates come from [`crate::write`], and the list SQL is what
//! `console::http::list` composes.

mod flow;

pub use flow::{
    EnrichmentCredential, FlowCredential, FlowCredentialWrite, FlowUpsertError, NewFlowCredential,
    WrappingRule, WrappingSecretSpec, apply_enriched_identity, credential_expires_at,
    credential_for_app_subject, find_enrichment_credential, first_credential_for_app,
    upsert_flow_credential, upsert_wrapping_secret,
};

use std::collections::HashMap;

use active_record_encryption::ActiveRecordEncryption;
use serde_json::{Map, Value};
use sqlx::{PgPool, types::Json};
use time::{Duration, PrimitiveDateTime};

use crate::Result;
use crate::invalidate::{self, Scope};
use crate::model::{
    self, BrokerCredential as BrokerCredentialModel, ConsoleModel, OauthApp as OauthAppModel,
    SecretSource as SecretSourceModel, StaticSecret as StaticSecretModel,
};
use crate::timestamp;
use crate::write;

/// The shortest early-refresh slack, in seconds.
pub const REFRESH_FLOOR_SECONDS: i64 = 60;
/// The default credential lifetime, in seconds.
pub const DEFAULT_EXPIRES_IN_SECONDS: i64 = 5 * 60;
/// The default early-refresh slack, in seconds.
pub const DEFAULT_EARLY_REFRESH_SLACK_SECONDS: i32 = 300;
/// The default early-refresh fraction of the credential's lifetime.
pub const DEFAULT_EARLY_REFRESH_FRACTION: f64 = 0.2;
/// The default ceiling between refreshes, in seconds.
pub const DEFAULT_MAX_REFRESH_INTERVAL_SECONDS: i32 = 86_400;
/// The Preqin token endpoint.
pub const PREQIN_TOKEN_ENDPOINT: &str = "https://api.preqin.com/connect/token";
/// The dead reason recorded on disconnect.
pub const DISCONNECTED_DEAD_REASON: &str = "disconnected";
/// The URL-safe identifier format shared by OAuth apps and broker credentials.
pub const URL_SAFE_MESSAGE: &str =
    "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)";

pub fn url_safe(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

/// Active Record encryption for the write-only attributes in this area, keyed
/// from the same `CENTAUR_CONSOLE_AR_ENCRYPTION_*` settings.
pub struct OauthEncryption {
    inner: ActiveRecordEncryption,
}

impl std::fmt::Debug for OauthEncryption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OauthEncryption")
            .finish_non_exhaustive()
    }
}

impl OauthEncryption {
    pub fn new(primary_key: &str, salt: &str) -> Self {
        Self {
            inner: ActiveRecordEncryption::new(primary_key, salt),
        }
    }

    /// The deployment's keys. Absent keys mean encrypted attributes cannot be
    /// read or written; handlers treat that as a configuration failure rather
    /// than silently storing plaintext. The environment is read in
    /// `areas::encryption` only.
    pub fn from_env() -> Option<Self> {
        let (primary_key, salt) = super::encryption::keys().ok()?;
        Some(Self::new(&primary_key, &salt))
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<String> {
        Ok(self.inner.encrypt(plaintext)?)
    }

    pub fn decrypt(&self, ciphertext: &str) -> Result<String> {
        Ok(self.inner.decrypt(ciphertext)?)
    }
}

/// One row of `oauth_apps`. `client_secret` keeps the encrypted value; only
/// token exchange and re-encryption need the plaintext.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct OauthApp {
    pub id: i64,
    pub slug: String,
    pub description: Option<String>,
    #[sqlx(json)]
    pub labels: Map<String, Value>,
    pub provider: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    #[sqlx(json)]
    pub allowed_scopes: Vec<String>,
    #[sqlx(json)]
    pub provider_config: Map<String, Value>,
    pub enabled: bool,
    pub always_available: bool,
    pub shared: bool,
    pub owner_only: bool,
    pub created_by_id: i64,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

/// The app list's projection and filter, shared by the console and bearer
/// indexes. The label filter binds `$1`; `console::http::list` appends the
/// ordering and the page bounds.
pub const APP_LIST_SELECT: &str = "SELECT *";
pub const APP_LIST_FROM: &str = "FROM oauth_apps WHERE $1::jsonb IS NULL OR labels @> $1::jsonb";
pub const APP_LIST_ORDER: &str = "ORDER BY created_at ASC, id ASC";

/// The app a slug names. A slug is not a `foreign_id`, so this keeps its own
/// `WHERE`; the descriptor supplies the table and the 404.
pub async fn find_oauth_app_by_slug(pool: &PgPool, slug: &str) -> Result<Option<OauthApp>> {
    Ok(sqlx::query_as(&format!(
        "SELECT * FROM {} WHERE slug = $1",
        OauthAppModel::TABLE
    ))
    .bind(slug)
    .fetch_optional(pool)
    .await?)
}

/// Serializes consents for a shared app, which may hold only one credential.
pub async fn lock_oauth_app_row(connection: &mut sqlx::PgConnection, slug: &str) -> Result<()> {
    sqlx::query(&format!(
        "SELECT id FROM {} WHERE slug = $1 FOR UPDATE",
        OauthAppModel::TABLE
    ))
    .bind(slug)
    .execute(connection)
    .await?;
    Ok(())
}

/// Credential counts per app, for the console index.
pub async fn oauth_app_credential_counts(
    pool: &PgPool,
    app_ids: &[i64],
) -> Result<HashMap<i64, i64>> {
    let rows: Vec<(i64, i64)> = sqlx::query_as(&format!(
        "SELECT oauth_app_id, count(*) AS credentials_count \
         FROM {} WHERE oauth_app_id = ANY($1) GROUP BY oauth_app_id",
        BrokerCredentialModel::TABLE
    ))
    .bind(app_ids)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

pub async fn oauth_app_credentials_count(pool: &PgPool, app_id: i64) -> Result<i64> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {} WHERE oauth_app_id = $1",
        BrokerCredentialModel::TABLE
    ))
    .bind(app_id)
    .fetch_one(pool)
    .await?)
}

pub async fn all_oauth_app_slugs(pool: &PgPool) -> Result<Vec<String>> {
    Ok(
        sqlx::query_scalar(&format!("SELECT slug FROM {}", OauthAppModel::TABLE))
            .fetch_all(pool)
            .await?,
    )
}

pub async fn all_oauth_app_providers(pool: &PgPool) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT DISTINCT provider FROM {}",
        OauthAppModel::TABLE
    ))
    .fetch_all(pool)
    .await?)
}

/// Enabled apps for the Integrations page, in slug order. Non-admins only see
/// the apps that are not shared.
pub async fn enabled_oauth_apps(pool: &PgPool, include_shared: bool) -> Result<Vec<OauthApp>> {
    Ok(sqlx::query_as(&format!(
        "SELECT * FROM {} WHERE enabled = TRUE AND (shared = FALSE OR $1) \
         ORDER BY slug ASC, id ASC",
        OauthAppModel::TABLE
    ))
    .bind(include_shared)
    .fetch_all(pool)
    .await?)
}

/// A new `oauth_apps` row. Timestamps come from the insert helper.
pub struct NewOauthApp {
    pub slug: String,
    pub description: Option<String>,
    pub labels: Map<String, Value>,
    pub provider: String,
    pub client_id: String,
    pub client_secret: String,
    pub allowed_scopes: Vec<String>,
    pub provider_config: Map<String, Value>,
    pub enabled: bool,
    pub always_available: bool,
    pub shared: bool,
    pub owner_only: bool,
    pub created_by_id: i64,
}

pub async fn insert_oauth_app(pool: &PgPool, app: &NewOauthApp) -> Result<i64> {
    let sql = write::insert_returning_sql(
        OauthAppModel::TABLE,
        &[
            "slug",
            "description",
            "labels",
            "provider",
            "client_id",
            "client_secret",
            "allowed_scopes",
            "provider_config",
            "enabled",
            "always_available",
            "shared",
            "owner_only",
            "created_by_id",
        ],
        "id",
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(&app.slug)
        .bind(&app.description)
        .bind(Json(&app.labels))
        .bind(&app.provider)
        .bind(&app.client_id)
        .bind(&app.client_secret)
        .bind(Json(&app.allowed_scopes))
        .bind(Json(&app.provider_config))
        .bind(app.enabled)
        .bind(app.always_available)
        .bind(app.shared)
        .bind(app.owner_only)
        .bind(app.created_by_id)
        .fetch_one(pool)
        .await?)
}

/// Replaces every mutable column of an existing app. `client_secret` is the
/// encrypted value to store: the stored one when the body carried no new
/// secret, `None` when the row has none.
pub async fn update_oauth_app(
    pool: &PgPool,
    id: i64,
    app: &NewOauthApp,
    client_secret: Option<&str>,
) -> Result<()> {
    let sql = write::update_sql(
        OauthAppModel::TABLE,
        &[
            "slug",
            "description",
            "labels",
            "provider",
            "client_id",
            "client_secret",
            "allowed_scopes",
            "provider_config",
            "enabled",
            "always_available",
            "shared",
            "owner_only",
        ],
    );
    sqlx::query(&sql)
        .bind(&app.slug)
        .bind(&app.description)
        .bind(Json(&app.labels))
        .bind(&app.provider)
        .bind(&app.client_id)
        .bind(client_secret)
        .bind(Json(&app.allowed_scopes))
        .bind(Json(&app.provider_config))
        .bind(app.enabled)
        .bind(app.always_available)
        .bind(app.shared)
        .bind(app.owner_only)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

/// One row of `broker_credentials`. The encrypted columns keep their stored
/// envelope; callers decrypt with [`OauthEncryption`] when they need values.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct BrokerCredential {
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    #[sqlx(json)]
    pub labels: Map<String, Value>,
    pub grant: String,
    pub token_endpoint: String,
    #[sqlx(json)]
    pub scopes: Vec<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub api_key: Option<String>,
    /// Encrypted JSON: the column holds an encrypted JSON document of header
    /// names to values.
    pub token_endpoint_headers: Option<String>,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<PrimitiveDateTime>,
    pub last_refresh: Option<PrimitiveDateTime>,
    pub next_attempt_at: Option<PrimitiveDateTime>,
    pub dead: bool,
    pub dead_reason: Option<String>,
    pub failure_count: i32,
    pub early_refresh_fraction: f64,
    pub early_refresh_slack_seconds: i32,
    pub max_refresh_interval_seconds: i32,
    pub refresh_timeout_seconds: i32,
    pub oauth_app_id: Option<i64>,
    pub provider_subject: Option<String>,
    pub provider_email: Option<String>,
    pub external_user_key: Option<String>,
    pub created_by_id: Option<i64>,
    pub created_at: PrimitiveDateTime,
    pub updated_at: PrimitiveDateTime,
}

/// The credential list's projection and filter, shared by the console and
/// bearer indexes. The label filter binds `$1`.
pub const CREDENTIAL_LIST_SELECT: &str = "SELECT *";
pub const CREDENTIAL_LIST_FROM: &str =
    "FROM broker_credentials WHERE $1::jsonb IS NULL OR labels @> $1::jsonb";
pub const CREDENTIAL_LIST_ORDER: &str = "ORDER BY created_at ASC, id ASC";

/// The credentials of one app: the app id binds `$1` and the label filter `$2`.
pub const APP_CREDENTIAL_LIST_SELECT: &str = "SELECT *";
pub const APP_CREDENTIAL_LIST_FROM: &str = "FROM broker_credentials \
     WHERE oauth_app_id = $1 AND ($2::jsonb IS NULL OR labels @> $2::jsonb)";
pub const APP_CREDENTIAL_LIST_ORDER: &str = "ORDER BY created_at ASC, id ASC";

impl BrokerCredential {
    /// The computed status: `dead`, `bootstrapping` (never refreshed), or
    /// `live`.
    pub fn status(&self) -> &'static str {
        if self.dead {
            "dead"
        } else if self.last_refresh.is_none() {
            "bootstrapping"
        } else {
            "live"
        }
    }

    /// The decrypted `token_endpoint_headers` object. A malformed value is an
    /// invalid row, not a credential that can be refreshed with.
    pub fn token_endpoint_headers(
        &self,
        encryption: &OauthEncryption,
    ) -> Result<Map<String, Value>> {
        let Some(ciphertext) = &self.token_endpoint_headers else {
            return Ok(Map::new());
        };
        let json = encryption.decrypt(ciphertext)?;
        match serde_json::from_str::<Value>(&json) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(Value::Null) => Ok(Map::new()),
            _ => Err(crate::Error::InvalidData(
                "token_endpoint_headers is not a JSON object".to_owned(),
            )),
        }
    }

    pub fn token_endpoint_header_names(&self, encryption: &OauthEncryption) -> Result<Vec<String>> {
        Ok(self
            .token_endpoint_headers(encryption)?
            .keys()
            .cloned()
            .collect())
    }
}

/// The first credential an app minted, oldest first. The console's
/// provider-details panel reads it for a shared app.
pub async fn first_credential(pool: &PgPool, app_id: i64) -> Result<Option<BrokerCredential>> {
    Ok(sqlx::query_as(&format!(
        "SELECT * FROM {} WHERE oauth_app_id = $1 ORDER BY created_at ASC, id ASC LIMIT 1",
        BrokerCredentialModel::TABLE
    ))
    .bind(app_id)
    .fetch_optional(pool)
    .await?)
}

/// `StaticSecret` summary attached to a broker credential.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct WrappingSecret {
    pub id: i64,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
}

pub async fn wrapping_secret_for_credential(
    pool: &PgPool,
    credential_id: i64,
) -> Result<Option<WrappingSecret>> {
    Ok(sqlx::query_as(&format!(
        "SELECT id, foreign_id, name FROM {} WHERE broker_credential_id = $1",
        StaticSecretModel::TABLE
    ))
    .bind(credential_id)
    .fetch_optional(pool)
    .await?)
}

/// The app summary a credential payload embeds.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct CredentialOauthApp {
    pub id: i64,
    pub slug: String,
    pub provider: String,
    pub client_id: String,
    #[sqlx(json)]
    pub provider_config: Map<String, Value>,
}

pub async fn oauth_app_for_credential(
    pool: &PgPool,
    app_id: i64,
) -> Result<Option<CredentialOauthApp>> {
    Ok(sqlx::query_as(&format!(
        "SELECT id, slug, provider, client_id, provider_config FROM {} WHERE id = $1",
        OauthAppModel::TABLE
    ))
    .bind(app_id)
    .fetch_optional(pool)
    .await?)
}

/// A user's connections on the Integrations page: credentials they minted,
/// ones whose IdP email matches their console login, and (for shared apps) any
/// admin-connected account. Oldest first, so the caller's newest-wins map keeps
/// the last connection per app.
pub async fn connections_for_user(
    pool: &PgPool,
    user_id: i64,
    email: &str,
    app_ids: &[i64],
    shared_app_ids: &[i64],
) -> Result<Vec<BrokerCredential>> {
    Ok(sqlx::query_as(&format!(
        "SELECT * FROM {} \
         WHERE oauth_app_id = ANY($3) \
           AND (created_by_id = $1 OR provider_email = $2 OR oauth_app_id = ANY($4)) \
           AND dead_reason IS DISTINCT FROM $5 \
         ORDER BY updated_at ASC, id ASC",
        BrokerCredentialModel::TABLE
    ))
    .bind(user_id)
    .bind(email)
    .bind(app_ids)
    .bind(shared_app_ids)
    .bind(DISCONNECTED_DEAD_REASON)
    .fetch_all(pool)
    .await?)
}

/// A `broker_credentials` row written by the CRUD handlers. Every
/// `encrypts`-backed field holds the encrypted envelope the handler built with
/// [`OauthEncryption`], matching Active Record. `id` selects an update; an
/// absent `id` inserts and uses `created_by_id`.
pub struct BrokerCredentialWrite {
    pub id: Option<i64>,
    pub foreign_id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub labels: Map<String, Value>,
    pub grant: String,
    pub token_endpoint: String,
    pub scopes: Vec<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub api_key: Option<String>,
    pub token_endpoint_headers: Option<String>,
    pub refresh_token: Option<String>,
    pub early_refresh_fraction: f64,
    pub early_refresh_slack_seconds: i32,
    pub max_refresh_interval_seconds: i32,
    pub refresh_timeout_seconds: i32,
    /// A freshly supplied write-only value puts the credential back in the
    /// "due now" state.
    pub reset_refresh_state: bool,
    pub created_by_id: Option<i64>,
}

pub async fn insert_broker_credential(
    pool: &PgPool,
    credential: &BrokerCredentialWrite,
) -> Result<i64> {
    let sql = write::insert_returning_sql(
        BrokerCredentialModel::TABLE,
        &[
            "foreign_id",
            "name",
            "description",
            "labels",
            "grant",
            "token_endpoint",
            "scopes",
            "client_id",
            "client_secret",
            "username",
            "password",
            "api_key",
            "token_endpoint_headers",
            "refresh_token",
            "early_refresh_slack_seconds",
            "early_refresh_fraction",
            "max_refresh_interval_seconds",
            "refresh_timeout_seconds",
            "next_attempt_at",
            "created_by_id",
        ],
        "id",
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(&credential.foreign_id)
        .bind(&credential.name)
        .bind(&credential.description)
        .bind(Json(&credential.labels))
        .bind(&credential.grant)
        .bind(&credential.token_endpoint)
        .bind(Json(&credential.scopes))
        .bind(&credential.client_id)
        .bind(&credential.client_secret)
        .bind(&credential.username)
        .bind(&credential.password)
        .bind(&credential.api_key)
        .bind(&credential.token_endpoint_headers)
        .bind(&credential.refresh_token)
        .bind(credential.early_refresh_slack_seconds)
        .bind(credential.early_refresh_fraction)
        .bind(credential.max_refresh_interval_seconds)
        .bind(credential.refresh_timeout_seconds)
        .bind(credential.reset_refresh_state.then(timestamp::now))
        .bind(credential.created_by_id)
        .fetch_one(pool)
        .await?)
}

/// The replace-style write the CRUD handlers apply to an existing credential.
/// The handler merges the existing row with the body (so an omitted key keeps
/// the stored value and an explicit null clears it) and hands the complete
/// result here; encrypted fields are already ciphertext.
pub async fn update_broker_credential(
    pool: &PgPool,
    id: i64,
    values: &BrokerCredentialWrite,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let was_dead: Option<bool> = sqlx::query_scalar(&format!(
        "SELECT dead FROM {} WHERE id = $1 FOR UPDATE",
        BrokerCredentialModel::TABLE
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let sql = write::update_sql(
        BrokerCredentialModel::TABLE,
        &[
            "foreign_id",
            "name",
            "description",
            "labels",
            "grant",
            "token_endpoint",
            "scopes",
            "client_id",
            "client_secret",
            "username",
            "password",
            "api_key",
            "token_endpoint_headers",
            "refresh_token",
            "early_refresh_fraction",
            "early_refresh_slack_seconds",
            "max_refresh_interval_seconds",
            "refresh_timeout_seconds",
        ],
    );
    sqlx::query(&sql)
        .bind(&values.foreign_id)
        .bind(&values.name)
        .bind(&values.description)
        .bind(Json(&values.labels))
        .bind(&values.grant)
        .bind(&values.token_endpoint)
        .bind(Json(&values.scopes))
        .bind(&values.client_id)
        .bind(&values.client_secret)
        .bind(&values.username)
        .bind(&values.password)
        .bind(&values.api_key)
        .bind(&values.token_endpoint_headers)
        .bind(&values.refresh_token)
        .bind(values.early_refresh_fraction)
        .bind(values.early_refresh_slack_seconds)
        .bind(values.max_refresh_interval_seconds)
        .bind(values.refresh_timeout_seconds)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if values.reset_refresh_state {
        // `apply_initial_values` revives the credential: it is no longer dead
        // and is due for a refresh now.
        let sql = write::update_sql(
            BrokerCredentialModel::TABLE,
            &["dead", "dead_reason", "failure_count", "next_attempt_at"],
        );
        sqlx::query(&sql)
            .bind(false)
            .bind(Option::<String>::None)
            .bind(0_i32)
            .bind(timestamp::now())
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    // Reviving a dead credential changes `dead`, which is a sync-config
    // relevant change, so the credential's scope is invalidated.
    if values.reset_refresh_state && was_dead == Some(true) {
        invalidate::invalidate(&mut tx, Scope::BrokerCredential(id)).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Clears the secret's `broker_credential_id`, then deletes the row.
pub async fn delete_broker_credential(pool: &PgPool, id: i64) -> Result<()> {
    let mut tx = pool.begin().await?;
    let sql = write::update_where_sql(
        StaticSecretModel::TABLE,
        &["broker_credential_id"],
        "broker_credential_id = $2",
    );
    sqlx::query(&sql)
        .bind(Option::<i64>::None)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    model::delete::<BrokerCredentialModel, _>(&mut *tx, id).await?;
    tx.commit().await?;
    Ok(())
}

/// The plaintext tokens a disconnect must revoke, captured under the row lock
/// and cleared in the same transaction.
#[derive(Clone, Debug, Default)]
pub struct DisconnectedTokens {
    pub refresh_token: Option<String>,
    pub access_token: Option<String>,
}

/// Clear local access under the credential's row lock, then let the caller
/// revoke the captured tokens outside the transaction. Returns `None` when the
/// row is gone.
pub async fn disconnect_broker_credential(
    pool: &PgPool,
    id: i64,
    encryption: &OauthEncryption,
) -> Result<Option<DisconnectedTokens>> {
    let mut tx = pool.begin().await?;
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(&format!(
        "SELECT access_token, refresh_token FROM {} WHERE id = $1 FOR UPDATE",
        BrokerCredentialModel::TABLE
    ))
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((access_token, refresh_token)) = row else {
        tx.rollback().await?;
        return Ok(None);
    };
    let decrypt = |value: Option<String>| -> Result<Option<String>> {
        value
            .map(|ciphertext| encryption.decrypt(&ciphertext))
            .transpose()
    };
    let tokens = DisconnectedTokens {
        refresh_token: decrypt(refresh_token)?,
        access_token: decrypt(access_token)?,
    };
    let sql = write::update_sql(
        BrokerCredentialModel::TABLE,
        &[
            "access_token",
            "refresh_token",
            "expires_at",
            "dead",
            "dead_reason",
            "next_attempt_at",
        ],
    );
    sqlx::query(&sql)
        .bind(Option::<String>::None)
        .bind(Option::<String>::None)
        .bind(timestamp::now())
        .bind(true)
        .bind(DISCONNECTED_DEAD_REASON)
        .bind(Option::<PrimitiveDateTime>::None)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    // Clearing the tokens under the lock always changes `access_token`, `dead`,
    // and `expires_at`, so the credential's scope is always invalidated.
    invalidate::invalidate(&mut tx, Scope::BrokerCredential(id)).await?;
    tx.commit().await?;
    Ok(Some(tokens))
}

/// Whether any `token_broker` secret source still references the credential.
pub async fn credential_referenced_by_secret_source(pool: &PgPool, id: i64) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {} \
         WHERE source_type = 'token_broker' AND broker_credential_id = $1",
        SecretSourceModel::TABLE
    ))
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

/// Whether the user saved a personal SSH key, for the Integrations page.
pub async fn user_has_ssh_key(pool: &PgPool, user_id: i64) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {} WHERE owner_id = $1",
        model::SshKeySecret::TABLE
    ))
    .bind(user_id)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

/// The next refresh attempt: the early-refresh time, capped by the maximum
/// refresh interval.
pub fn compute_next_attempt_at(
    expires_at: Option<PrimitiveDateTime>,
    last_refresh: Option<PrimitiveDateTime>,
    early_refresh_slack_seconds: i32,
    early_refresh_fraction: f64,
    max_refresh_interval_seconds: i32,
    now: PrimitiveDateTime,
) -> PrimitiveDateTime {
    let (Some(expires_at), Some(last_refresh)) = (expires_at, last_refresh) else {
        return now;
    };
    let expires_at = timestamp::offset(expires_at);
    let last_refresh = timestamp::offset(last_refresh);
    let mut slack = i64::from(early_refresh_slack_seconds);
    let ttl = (expires_at - last_refresh).whole_seconds();
    if early_refresh_fraction > 0.0 && ttl > 0 {
        let frac_slack = (ttl as f64 * early_refresh_fraction) as i64;
        if frac_slack > slack {
            slack = frac_slack;
        }
    }
    if slack < REFRESH_FLOOR_SECONDS {
        slack = REFRESH_FLOOR_SECONDS;
    }
    let early = expires_at - Duration::seconds(slack);
    let ceiling = last_refresh + Duration::seconds(i64::from(max_refresh_interval_seconds));
    timestamp::datetime(early.min(ceiling))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_attempt_uses_the_larger_slack_and_the_interval_ceiling() {
        let now = time::macros::datetime!(2024-03-04 00:00:00);
        let last = now;
        let expires = now + Duration::seconds(3600);
        // 20% of an hour is 720s, above the 300s floor.
        assert_eq!(
            compute_next_attempt_at(Some(expires), Some(last), 300, 0.2, 86_400, now),
            now + Duration::seconds(3600 - 720)
        );
        // The 60s floor wins over a tiny fraction, so a credential expiring in
        // 30s is already past its early-refresh time. The result is not clamped
        // to the current time.
        assert_eq!(
            compute_next_attempt_at(
                Some(now + Duration::seconds(30)),
                Some(last),
                0,
                0.0,
                86_400,
                now
            ),
            now - Duration::seconds(30)
        );
        // The max-refresh-interval ceiling caps a long early window: with a
        // one-hour slack on a two-hour token, refreshing every 50 minutes wins.
        assert_eq!(
            compute_next_attempt_at(
                Some(now + Duration::seconds(7_200)),
                Some(last),
                3_600,
                0.0,
                3_000,
                now
            ),
            now + Duration::seconds(3_000)
        );
        // A never-refreshed credential is due immediately.
        assert_eq!(
            compute_next_attempt_at(Some(expires), None, 300, 0.2, 86_400, now),
            now
        );
    }
}
