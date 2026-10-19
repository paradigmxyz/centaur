//! Reads and writes Console's `broker_credentials`.

use std::time::Duration;

use active_record_encryption::ActiveRecordEncryption;
use chrono::NaiveDateTime;
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row, postgres::PgRow, types::Json};

use crate::{
    client::MAX_REQUEST_TIMEOUT, grants::Credential, identifiers::oid, schedule::Schedule,
};

pub(crate) type Error = Box<dyn std::error::Error + Send + Sync>;

/// Console's `BrokerCredential.refreshable` with `$1` as the current time: live
/// credentials that are due and have what their grant needs to refresh.
const REFRESHABLE: &str = "broker_credentials.dead = FALSE \
     AND ((broker_credentials.\"grant\" = 'refresh_token' \
           AND (broker_credentials.last_refresh IS NULL OR broker_credentials.refresh_token IS NOT NULL)) \
          OR broker_credentials.\"grant\" IN ('client_credentials', 'password', 'preqin')) \
     AND (broker_credentials.next_attempt_at IS NULL OR broker_credentials.next_attempt_at <= $1)";

pub(crate) async fn due_credential_ids(
    pool: &PgPool,
    now: NaiveDateTime,
) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar(&format!(
        "SELECT id FROM broker_credentials WHERE {REFRESHABLE} \
         ORDER BY next_attempt_at NULLS FIRST, id"
    ))
    .bind(now)
    .fetch_all(pool)
    .await
}

pub(crate) struct CredentialCounts {
    pub provider: Option<String>,
    pub token_endpoint: String,
    pub dead: i64,
    pub backing_off: i64,
}

/// Dead credentials, and live ones whose last refresh failed, by OAuth app
/// provider and token endpoint.
pub(crate) async fn credential_counts(pool: &PgPool) -> Result<Vec<CredentialCounts>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT oauth_apps.provider, broker_credentials.token_endpoint, \
                count(*) FILTER (WHERE broker_credentials.dead) AS dead, \
                count(*) FILTER (WHERE NOT broker_credentials.dead \
                                 AND broker_credentials.failure_count > 0) AS backing_off \
         FROM broker_credentials \
         LEFT JOIN oauth_apps ON oauth_apps.id = broker_credentials.oauth_app_id \
         GROUP BY 1, 2",
    )
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(CredentialCounts {
                provider: row.try_get("provider")?,
                token_endpoint: row.try_get("token_endpoint")?,
                dead: row.try_get("dead")?,
                backing_off: row.try_get("backing_off")?,
            })
        })
        .collect()
}

pub(crate) struct DueCredential {
    pub credential: Credential,
    pub schedule: Schedule,
    pub failure_count: i32,
}

/// Locks a credential that is still due, skipping it when another refresh or
/// a Console edit holds its row. The lock is the single-writer guarantee: a
/// refresh token is never used by two refreshes at once.
pub(crate) async fn lock_due_credential(
    connection: &mut PgConnection,
    encryption: &ActiveRecordEncryption,
    id: i64,
    now: NaiveDateTime,
) -> Result<Option<DueCredential>, Error> {
    let row = sqlx::query(&format!(
        "SELECT broker_credentials.\"grant\", broker_credentials.token_endpoint, \
                broker_credentials.client_id, broker_credentials.client_secret, \
                broker_credentials.refresh_token, broker_credentials.username, \
                broker_credentials.password, broker_credentials.api_key, \
                broker_credentials.token_endpoint_headers, broker_credentials.scopes, \
                broker_credentials.refresh_timeout_seconds, broker_credentials.failure_count, \
                broker_credentials.early_refresh_slack_seconds, \
                broker_credentials.early_refresh_fraction, \
                broker_credentials.max_refresh_interval_seconds, \
                oauth_apps.id IS NOT NULL AS has_oauth_app, oauth_apps.provider, \
                oauth_apps.client_id AS oauth_app_client_id, \
                oauth_apps.client_secret AS oauth_app_client_secret \
         FROM broker_credentials \
         LEFT JOIN oauth_apps ON oauth_apps.id = broker_credentials.oauth_app_id \
         WHERE broker_credentials.id = $2 AND {REFRESHABLE} \
         FOR UPDATE OF broker_credentials SKIP LOCKED"
    ))
    .bind(now)
    .bind(id)
    .fetch_optional(connection)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let decrypt = |column: &str| -> Result<Option<String>, Error> {
        row.try_get::<Option<String>, _>(column)?
            .map(|value| encryption.decrypt(&value))
            .transpose()
            .map_err(|error| format!("decrypting {column}: {error}").into())
    };
    // Flow-minted credentials use their app's client, so rotating the app's
    // secret applies to every credential it minted.
    let (client_id, client_secret) = if row.try_get("has_oauth_app")? {
        (
            row.try_get("oauth_app_client_id")?,
            decrypt("oauth_app_client_secret")?,
        )
    } else {
        (row.try_get("client_id")?, decrypt("client_secret")?)
    };
    let refresh_timeout_seconds: i32 = row.try_get("refresh_timeout_seconds")?;
    Ok(Some(DueCredential {
        credential: Credential {
            oid: oid("bcr", id),
            grant: row.try_get("grant")?,
            token_endpoint: row.try_get("token_endpoint")?,
            provider: row.try_get("provider")?,
            client_id,
            client_secret,
            refresh_token: decrypt("refresh_token")?,
            username: decrypt("username")?,
            password: decrypt("password")?,
            api_key: decrypt("api_key")?,
            token_endpoint_headers: headers(decrypt("token_endpoint_headers")?)?,
            scopes: row.try_get::<Json<Vec<String>>, _>("scopes")?.0,
            timeout: Duration::from_secs(refresh_timeout_seconds.max(1) as u64)
                .min(MAX_REQUEST_TIMEOUT),
        },
        schedule: schedule(&row)?,
        failure_count: row.try_get("failure_count")?,
    }))
}

fn schedule(row: &PgRow) -> Result<Schedule, sqlx::Error> {
    Ok(Schedule {
        early_refresh_slack_seconds: row.try_get("early_refresh_slack_seconds")?,
        early_refresh_fraction: row.try_get("early_refresh_fraction")?,
        max_refresh_interval_seconds: row.try_get("max_refresh_interval_seconds")?,
    })
}

/// `token_endpoint_headers` is a JSON object of header names to values.
fn headers(json: Option<String>) -> Result<Vec<(String, String)>, Error> {
    let invalid = || "token_endpoint_headers is not an object of strings".into();
    match json.map(|json| serde_json::from_str(&json)).transpose() {
        Ok(None | Some(Value::Null)) => Ok(Vec::new()),
        Ok(Some(Value::Object(headers))) => headers
            .into_iter()
            .map(|(name, value)| match value {
                Value::String(value) => Ok((name, value)),
                _ => Err(invalid()),
            })
            .collect(),
        _ => Err(invalid()),
    }
}

pub(crate) enum RefreshTokenChange {
    Keep,
    Replace(String),
    Clear,
}

pub(crate) struct Refreshed<'a> {
    pub access_token: &'a str,
    pub refresh_token: RefreshTokenChange,
    pub expires_at: NaiveDateTime,
    pub next_attempt_at: NaiveDateTime,
}

pub(crate) async fn record_refresh(
    connection: &mut PgConnection,
    encryption: &ActiveRecordEncryption,
    id: i64,
    now: NaiveDateTime,
    refreshed: Refreshed<'_>,
) -> Result<(), Error> {
    let (change_refresh_token, refresh_token) = match &refreshed.refresh_token {
        RefreshTokenChange::Keep => (false, None),
        RefreshTokenChange::Replace(token) => (true, Some(encryption.encrypt(token)?)),
        RefreshTokenChange::Clear => (true, None),
    };
    sqlx::query(
        "UPDATE broker_credentials \
         SET access_token = $2, expires_at = $3, last_refresh = $4, updated_at = $4, \
             next_attempt_at = $5, failure_count = 0, dead = FALSE, dead_reason = NULL, \
             refresh_token = CASE WHEN $6 THEN $7 ELSE refresh_token END \
         WHERE id = $1",
    )
    .bind(id)
    .bind(encryption.encrypt(refreshed.access_token)?)
    .bind(refreshed.expires_at)
    .bind(now)
    .bind(refreshed.next_attempt_at)
    .bind(change_refresh_token)
    .bind(refresh_token)
    .execute(&mut *connection)
    .await?;
    invalidate_proxy_configs(connection, id).await?;
    Ok(())
}

pub(crate) async fn record_retry(
    connection: &mut PgConnection,
    id: i64,
    now: NaiveDateTime,
    failure_count: i32,
    next_attempt_at: NaiveDateTime,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE broker_credentials \
         SET failure_count = $2, next_attempt_at = $3, updated_at = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(failure_count)
    .bind(next_attempt_at)
    .bind(now)
    .execute(connection)
    .await?;
    Ok(())
}

pub(crate) async fn record_dead(
    connection: &mut PgConnection,
    id: i64,
    now: NaiveDateTime,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE broker_credentials \
         SET dead = TRUE, dead_reason = $2, updated_at = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(reason)
    .bind(now)
    .execute(&mut *connection)
    .await?;
    invalidate_proxy_configs(connection, id).await
}

/// Bumps the sync config cache version of every principal granted, directly or
/// through a role, a secret whose `token_broker` source delivers this
/// credential, so proxy-sync rebuilds their proxy configuration.
async fn invalidate_proxy_configs(
    connection: &mut PgConnection,
    id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "WITH owner_grants AS ( \
             SELECT grants.principal_id, grants.role_id \
             FROM secret_sources sources \
             JOIN grants ON grants.static_secret_id = sources.static_secret_id \
                 OR grants.gcp_auth_secret_id = sources.gcp_auth_secret_id \
                 OR grants.gcp_id_token_secret_id = sources.gcp_id_token_secret_id \
                 OR grants.aws_auth_secret_id = sources.aws_auth_secret_id \
                 OR grants.oauth_token_secret_id = sources.oauth_token_secret_id \
                 OR grants.pg_dsn_secret_id = sources.pg_dsn_secret_id \
                 OR grants.hmac_secret_id = sources.hmac_secret_id \
             WHERE sources.source_type = 'token_broker' AND sources.broker_credential_id = $1 \
         ) \
         UPDATE principals SET sync_config_cache_version = sync_config_cache_version + 1 \
         WHERE id IN (SELECT principal_id FROM owner_grants) \
            OR id IN (SELECT principal_roles.principal_id FROM principal_roles \
                      JOIN owner_grants ON owner_grants.role_id = principal_roles.role_id)",
    )
    .bind(id)
    .execute(connection)
    .await?;
    Ok(())
}
