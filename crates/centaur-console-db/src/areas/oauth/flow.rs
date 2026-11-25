//! The writes the OAuth consent flow performs: one credential per
//! `(app, provider account)`, revived and re-tokened on every consent, plus the
//! grantable static secret that wraps it.
//!
//! [`FlowCredential`] derives [`sqlx::FromRow`], the inserts and updates come
//! from [`crate::write`], and the credential row's descriptor supplies the table
//! and the delete.

use serde_json::{Map, Value};
use sqlx::{PgConnection, PgPool, types::Json};
use time::PrimitiveDateTime;

use super::{OauthApp, OauthEncryption};
use crate::areas::secrets::SecretKind;
use crate::invalidate::{self, Scope};
use crate::model::{
    BrokerCredential as BrokerCredentialModel, ConsoleModel, OauthApp as OauthAppModel,
    RequestRule as RequestRuleModel, SecretSource as SecretSourceModel,
    StaticSecret as StaticSecretModel,
};
use crate::timestamp;
use crate::write;

/// A failure while upserting a consent-flow credential. Only `AccountMismatch`
/// and `UniqueViolation` map to their own user-facing messages; everything else
/// is an internal error.
#[derive(Debug)]
pub enum FlowUpsertError {
    Console(crate::Error),
    /// A shared app already has an account, or an owner-only credential belongs
    /// to someone else. The API surfaces this as `account_mismatch`.
    AccountMismatch,
    /// A concurrent consent won the unique index.
    UniqueViolation,
}

type Result<T> = std::result::Result<T, FlowUpsertError>;

impl From<sqlx::Error> for FlowUpsertError {
    fn from(error: sqlx::Error) -> Self {
        let error = crate::Error::Database(error);
        if error.is_unique_violation() {
            Self::UniqueViolation
        } else {
            Self::Console(error)
        }
    }
}

impl From<crate::Error> for FlowUpsertError {
    fn from(error: crate::Error) -> Self {
        match error {
            crate::Error::Database(error) => Self::from(error),
            other => Self::Console(other),
        }
    }
}

impl std::fmt::Display for FlowUpsertError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Console(error) => error.fmt(formatter),
            Self::AccountMismatch => formatter.write_str("consent belongs to another account"),
            Self::UniqueViolation => {
                formatter.write_str("a concurrent consent won the unique index")
            }
        }
    }
}

impl std::error::Error for FlowUpsertError {}

/// The values a consent applies to an existing or new credential.
pub struct FlowCredentialWrite {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<PrimitiveDateTime>,
    pub last_refresh: PrimitiveDateTime,
    pub next_attempt_at: Option<PrimitiveDateTime>,
    pub scopes: Vec<String>,
    pub labels: Map<String, Value>,
    pub provider_email: Option<String>,
    /// Set only for owner-only apps, which keep the credential's endpoint in
    /// step with the app rather than the provider account.
    pub token_endpoint: Option<String>,
}

/// The identity fields a newly minted credential needs.
pub struct NewFlowCredential {
    pub provider_subject: String,
    pub foreign_id: String,
    pub display_name: String,
    pub token_endpoint: String,
    pub external_user_key: String,
    pub created_by_id: i64,
}

/// The projection every consent needs from an existing credential.
pub const FLOW_COLUMNS: &str = "id, name, provider_subject, created_by_id, labels, token_endpoint";

/// A credential minted by the flow.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct FlowCredential {
    pub id: i64,
    pub name: Option<String>,
    pub provider_subject: Option<String>,
    pub created_by_id: Option<i64>,
    /// The stored labels, which every consent merges the identity's labels into.
    #[sqlx(json)]
    pub labels: Map<String, Value>,
    pub token_endpoint: String,
}

/// The credential and wrapper secret the enrichment job reads after a Slack
/// consent. The token stays encrypted; the caller decrypts it only to make the
/// profile requests.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct EnrichmentCredential {
    pub id: i64,
    pub name: Option<String>,
    pub provider_subject: Option<String>,
    pub provider_email: Option<String>,
    pub access_token: Option<String>,
    #[sqlx(json)]
    pub scopes: Vec<String>,
    pub provider: String,
    pub static_secret_id: Option<i64>,
    pub static_secret_name: Option<String>,
}

/// The credential the enrichment job would find, with its app and wrapping
/// secret eagerly joined. The job silently does nothing when the row is gone,
/// so a missing row is `None` rather than an error.
pub async fn find_enrichment_credential(
    pool: &PgPool,
    credential_id: i64,
) -> Result<Option<EnrichmentCredential>> {
    Ok(sqlx::query_as(&format!(
        "SELECT c.id, c.name, c.provider_subject, c.provider_email, c.access_token, \
         c.scopes, a.provider, s.id AS static_secret_id, s.name AS static_secret_name \
         FROM {} c JOIN {} a ON a.id = c.oauth_app_id \
         LEFT JOIN {} s ON s.broker_credential_id = c.id WHERE c.id = $1",
        BrokerCredentialModel::TABLE,
        OauthAppModel::TABLE,
        StaticSecretModel::TABLE
    ))
    .bind(credential_id)
    .fetch_optional(pool)
    .await?)
}

/// Applies the Slack profile the enrichment job looked up: rename the credential
/// and fill its provider email, then rename the auto-created wrapper secret when
/// it still carries the generated name. Each update runs its own reconciliation
/// and the wrapper's sync-config invalidation, so mirror that order inside the
/// transaction.
pub async fn apply_enriched_identity(
    pool: &PgPool,
    credential: &EnrichmentCredential,
    name: &str,
    provider_email: Option<&str>,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    let sql = write::update_changed_sql(BrokerCredentialModel::TABLE, &["name", "provider_email"]);
    let changed = sqlx::query(&sql)
        .bind(name)
        .bind(provider_email)
        .bind(credential.id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    if changed {
        crate::areas::reconciliation::apply_for_credential(&mut tx, credential.id).await?;
    }

    if let Some(secret_id) = credential.static_secret_id {
        let old_name_present = credential
            .name
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
        let generated_name = format!("{} token", credential.name.as_deref().unwrap_or_default());
        let rename = !old_name_present
            || credential.static_secret_name.as_deref() == Some(generated_name.as_str());
        if rename {
            let sql = write::update_changed_sql(StaticSecretModel::TABLE, &["name"]);
            let secret_changed = sqlx::query(&sql)
                .bind(format!("{name} token"))
                .bind(secret_id)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                > 0;
            if secret_changed {
                // `StaticSecret` includes `SyncConfigCacheInvalidation`: a saved
                // change bumps each grantee once.
                invalidate::invalidate(
                    &mut tx,
                    Scope::Secret {
                        kind: SecretKind::Static,
                        id: secret_id,
                    },
                )
                .await?;
                crate::areas::reconciliation::apply_for_credential(&mut tx, credential.id).await?;
            }
        }
    }

    tx.commit().await?;
    Ok(())
}

/// The credential for one provider account on an app.
pub async fn credential_for_app_subject(
    connection: &mut PgConnection,
    app_id: i64,
    subject: &str,
) -> Result<Option<FlowCredential>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {FLOW_COLUMNS} FROM {} WHERE oauth_app_id = $1 AND provider_subject = $2",
        BrokerCredentialModel::TABLE
    ))
    .bind(app_id)
    .bind(subject)
    .fetch_optional(&mut *connection)
    .await?)
}

/// The first credential a shared app minted, oldest first.
pub async fn first_credential_for_app(
    connection: &mut PgConnection,
    app_id: i64,
) -> Result<Option<FlowCredential>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {FLOW_COLUMNS} FROM {} WHERE oauth_app_id = $1 \
         ORDER BY created_at ASC, id ASC LIMIT 1",
        BrokerCredentialModel::TABLE
    ))
    .bind(app_id)
    .fetch_optional(&mut *connection)
    .await?)
}

/// Upserts the credential for `(app, identity subject)`: a new row carries the
/// identity and endpoint; an existing one is relocked, revived and re-tokened.
pub async fn upsert_flow_credential(
    connection: &mut PgConnection,
    encryption: &OauthEncryption,
    app: &OauthApp,
    new: &NewFlowCredential,
    write_values: &FlowCredentialWrite,
) -> Result<FlowCredential> {
    let existing =
        credential_for_app_subject(&mut *connection, app.id, &new.provider_subject).await?;
    let access_token = encryption.encrypt(&write_values.access_token)?;
    let refresh_token = write_values
        .refresh_token
        .as_deref()
        .map(|token| encryption.encrypt(token))
        .transpose()?;
    let Some(existing) = existing else {
        let sql = write::insert_returning_sql(
            BrokerCredentialModel::TABLE,
            &[
                "oauth_app_id",
                "provider_subject",
                "provider_email",
                "foreign_id",
                "name",
                "token_endpoint",
                "external_user_key",
                "created_by_id",
                "access_token",
                "refresh_token",
                "scopes",
                "labels",
                "expires_at",
                "last_refresh",
                "next_attempt_at",
                "failure_count",
                "dead",
            ],
            FLOW_COLUMNS,
        );
        return Ok(sqlx::query_as(&sql)
            .bind(app.id)
            .bind(&new.provider_subject)
            .bind(&write_values.provider_email)
            .bind(&new.foreign_id)
            .bind(&new.display_name)
            .bind(&new.token_endpoint)
            .bind(&new.external_user_key)
            .bind(new.created_by_id)
            .bind(&access_token)
            .bind(&refresh_token)
            .bind(Json(&write_values.scopes))
            .bind(Json(&write_values.labels))
            .bind(write_values.expires_at)
            .bind(write_values.last_refresh)
            .bind(write_values.next_attempt_at)
            .bind(0_i32)
            .bind(false)
            .fetch_one(&mut *connection)
            .await?);
    };
    // Reload under the same row lock the token broker and disconnect use.
    sqlx::query(&format!(
        "SELECT id FROM {} WHERE id = $1 FOR UPDATE",
        BrokerCredentialModel::TABLE
    ))
    .bind(existing.id)
    .execute(&mut *connection)
    .await?;
    let sql = write::update_sql(
        BrokerCredentialModel::TABLE,
        &[
            "provider_email",
            "scopes",
            "labels",
            "refresh_token",
            "access_token",
            "expires_at",
            "last_refresh",
            "failure_count",
            "dead",
            "dead_reason",
            "next_attempt_at",
            "token_endpoint",
            "created_by_id",
        ],
    );
    sqlx::query(&sql)
        .bind(&write_values.provider_email)
        .bind(Json(&write_values.scopes))
        .bind(Json(&write_values.labels))
        .bind(&refresh_token)
        .bind(&access_token)
        .bind(write_values.expires_at)
        .bind(write_values.last_refresh)
        .bind(0_i32)
        .bind(false)
        .bind(Option::<String>::None)
        .bind(write_values.next_attempt_at)
        .bind(
            write_values
                .token_endpoint
                .clone()
                .unwrap_or_else(|| existing.token_endpoint.clone()),
        )
        .bind(existing.created_by_id.unwrap_or(new.created_by_id))
        .bind(existing.id)
        .execute(&mut *connection)
        .await?;
    // Re-consent rewrites access_token, expires_at, and last_refresh, so the
    // sync-config invalidation always runs on this update.
    invalidate::invalidate(&mut *connection, Scope::BrokerCredential(existing.id)).await?;
    Ok(existing)
}

/// One request rule on the wrapping secret.
#[derive(Clone, Debug)]
pub struct WrappingRule {
    pub host: String,
    pub http_methods: Vec<String>,
    pub paths: Vec<String>,
}

/// The static secret that makes a minted credential grantable. Created once and
/// left alone on re-consent, except for owner-only apps, whose wrapper is
/// rebuilt so its source and rules follow the app.
pub struct WrappingSecretSpec {
    pub name: String,
    pub labels: Map<String, Value>,
    pub kind: &'static str,
    pub inject_config: Option<Value>,
    pub replace_config: Option<Value>,
    pub rules: Vec<WrappingRule>,
    pub rebuild: bool,
}

/// Creates the wrapping secret for a credential, or rebuilds it when
/// `spec.rebuild` is set.
pub async fn upsert_wrapping_secret(
    connection: &mut PgConnection,
    credential_id: i64,
    spec: &WrappingSecretSpec,
) -> Result<()> {
    let existing: Option<(i64, Value)> = sqlx::query_as(&format!(
        "SELECT id, labels FROM {} WHERE broker_credential_id = $1",
        StaticSecretModel::TABLE
    ))
    .bind(credential_id)
    .fetch_optional(&mut *connection)
    .await?;

    let secret_id = match existing {
        Some((_, _)) if !spec.rebuild => return Ok(()),
        Some((id, stored_labels)) => {
            let mut labels = match stored_labels {
                Value::Object(map) => map,
                _ => Map::new(),
            };
            labels.extend(spec.labels.clone());
            let sql = write::update_sql(
                StaticSecretModel::TABLE,
                &["name", "labels", "kind", "inject_config", "replace_config"],
            );
            sqlx::query(&sql)
                .bind(&spec.name)
                .bind(Json(Value::Object(labels)))
                .bind(spec.kind)
                .bind(&spec.inject_config)
                .bind(&spec.replace_config)
                .bind(id)
                .execute(&mut *connection)
                .await?;
            sqlx::query(&format!(
                "DELETE FROM {} WHERE static_secret_id = $1",
                RequestRuleModel::TABLE
            ))
            .bind(id)
            .execute(&mut *connection)
            .await?;
            id
        }
        None => {
            let sql = write::insert_returning_sql(
                StaticSecretModel::TABLE,
                &[
                    "broker_credential_id",
                    "name",
                    "labels",
                    "kind",
                    "inject_config",
                    "replace_config",
                ],
                "id",
            );
            sqlx::query_scalar(&sql)
                .bind(credential_id)
                .bind(&spec.name)
                .bind(Json(&spec.labels))
                .bind(spec.kind)
                .bind(&spec.inject_config)
                .bind(&spec.replace_config)
                .fetch_one(&mut *connection)
                .await?
        }
    };

    for (position, rule) in spec.rules.iter().enumerate() {
        let sql = write::insert_sql(
            RequestRuleModel::TABLE,
            &[
                "static_secret_id",
                "host",
                "http_methods",
                "paths",
                "position",
            ],
        );
        sqlx::query(&sql)
            .bind(secret_id)
            .bind(&rule.host)
            .bind(Json(&rule.http_methods))
            .bind(Json(&rule.paths))
            .bind(position as i32)
            .execute(&mut *connection)
            .await?;
    }

    sqlx::query(&format!(
        "DELETE FROM {} WHERE static_secret_id = $1",
        SecretSourceModel::TABLE
    ))
    .bind(secret_id)
    .execute(&mut *connection)
    .await?;
    let sql = write::insert_sql(
        SecretSourceModel::TABLE,
        &[
            "static_secret_id",
            "source_type",
            "broker_credential_id",
            "config",
        ],
    );
    sqlx::query(&sql)
        .bind(secret_id)
        .bind("token_broker")
        .bind(credential_id)
        .bind(Json(
            serde_json::json!({ "credential_id": BrokerCredentialModel::oid(credential_id) }),
        ))
        .execute(&mut *connection)
        .await?;

    Ok(())
}

/// `credential_expires_at`: the IdP's `expires_in` when positive, otherwise a
/// conservative default for refreshable credentials.
pub fn credential_expires_at(
    expires_in: Option<i64>,
    refreshable: bool,
    now: PrimitiveDateTime,
) -> Option<PrimitiveDateTime> {
    if let Some(expires_in) = expires_in.filter(|seconds| *seconds > 0) {
        return Some(timestamp::datetime(
            timestamp::offset(now) + time::Duration::seconds(expires_in),
        ));
    }
    refreshable.then(|| {
        timestamp::datetime(
            timestamp::offset(now) + time::Duration::seconds(super::DEFAULT_EXPIRES_IN_SECONDS),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expires_at_prefers_the_idp_value_and_defaults_only_when_refreshable() {
        let now = time::macros::datetime!(2024-03-04 00:00:00);
        assert_eq!(
            credential_expires_at(Some(3600), true, now),
            Some(time::macros::datetime!(2024-03-04 01:00:00))
        );
        assert_eq!(
            credential_expires_at(None, true, now),
            Some(time::macros::datetime!(2024-03-04 00:05:00))
        );
        assert_eq!(credential_expires_at(Some(0), false, now), None);
        assert_eq!(credential_expires_at(None, false, now), None);
    }
}
