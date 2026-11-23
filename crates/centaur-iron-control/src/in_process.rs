//! In-process implementation of the iron-control admin surface against the
//! Console database.
//!
//! [`crate::IronControlClient`] routes each call through the same
//! `centaur_console_db` functions the Console handlers use, so validation,
//! reconciliation, cache invalidation, and timestamps match the Console API.
//!
//! Writes need an operator for Console's `created_by_id` columns: the Console
//! system user (`centaur_console_db::SYSTEM_USER_EMAIL`), created on first use.

use std::collections::BTreeMap;

use centaur_console_db::areas::admin::{
    ProxyChange, ProxyRow, admin_proxy, create_proxy as db_create_proxy,
    delete_proxy as db_delete_proxy, update_proxy as db_update_proxy,
};
use centaur_console_db::areas::principals::{
    GrantWrite, Grantee as DbGrantee, PermissionWrite, PrincipalError, PrincipalWrite, RoleWrite,
    assign_role as db_assign_role, create_grant as db_create_grant, grant_payload, list_grants,
    upsert_principal as db_upsert_principal, upsert_role as db_upsert_role,
    upsert_slack_channel_permission as db_upsert_slack_channel_permission,
};
use centaur_console_db::areas::secrets::{
    AwsAuthRequest, HmacRequest, OauthTokenRequest, SecretDocumentPayload, SecretKind, SecretRow,
    WriteDocument, WriteOptions, WriteTarget, write_secret as db_write_secret,
};
use centaur_console_db::find_principal;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use crate::database;
use crate::error::{IronControlError, Result};
use crate::models::{
    AwsAuthSecretInput, GcpAuthSecretInput, GcpIdTokenSecretInput, Grant, GrantSecret, Grantee,
    HmacSecretInput, IdentityInput, OAuthTokenSecretInput, PgDsnSecretInput, Principal,
    PrincipalInput, Proxy, Role, SecretRecord, SlackChannelPermissionInput, StaticSecretInput,
};

const PRINCIPAL_OID_PREFIX: &str = "prn_";
const ROLE_OID_PREFIX: &str = "role_";
const PROXY_OID_PREFIX: &str = "prx_";
const TOKEN_PREFIX: &str = "iprx_";

/// The Console system user api-rs's writes are attributed to.
pub(crate) async fn operator_id(pool: &PgPool) -> Result<i64> {
    centaur_console_db::system_user_id(pool)
        .await
        .map_err(IronControlError::ConsoleDb)
}

pub(crate) async fn upsert_principal(pool: &PgPool, input: &PrincipalInput) -> Result<Principal> {
    let operator = operator_id(pool).await?;
    let write = principal_write(input);
    db_upsert_principal(pool, &input.foreign_id, &write, operator)
        .await
        .map_err(|error| principal_error("upsert principal", error))?;
    database::principal(pool, &input.foreign_id).await
}

pub(crate) async fn upsert_slack_channel_permission(
    pool: &PgPool,
    principal: &str,
    input: &SlackChannelPermissionInput,
) -> Result<()> {
    let id = decode_oid(PRINCIPAL_OID_PREFIX, principal)
        .ok_or_else(|| not_found("principal", principal))?;
    let write = PermissionWrite {
        channel_id: Some(input.channel_id.clone()),
        upload_enabled: Some(input.upload_enabled),
        download_enabled: Some(input.download_enabled),
        history_enabled: Some(input.history_enabled),
    };
    db_upsert_slack_channel_permission(pool, DbGrantee::Principal(id), &write)
        .await
        .map(|_| ())
        .map_err(|error| principal_error("upsert slack channel permission", error))
}

pub(crate) async fn upsert_role(pool: &PgPool, input: &IdentityInput) -> Result<Role> {
    let operator = operator_id(pool).await?;
    let (view, _) = db_upsert_role(
        pool,
        &input.foreign_id,
        &RoleWrite {
            foreign_id: Some(input.foreign_id.clone()),
            name: Some(Some(input.name.clone())),
            labels: (!input.labels.is_empty()).then(|| string_map(&input.labels)),
            slack_channel_permissions: None,
        },
        operator,
    )
    .await
    .map_err(|error| principal_error("upsert role", error))?;
    role_from_view(view)
}

pub(crate) async fn assign_role(pool: &PgPool, principal: &str, role: &str) -> Result<()> {
    let principal_id = decode_oid(PRINCIPAL_OID_PREFIX, principal)
        .ok_or_else(|| not_found("principal", principal))?;
    if !role.starts_with(ROLE_OID_PREFIX) {
        return Err(not_found("role", role));
    }
    db_assign_role(pool, principal_id, role)
        .await
        .map(|_| ())
        .map_err(|error| principal_error("assign role", error))
}

pub(crate) async fn list_role_grants(pool: &PgPool, role: &str) -> Result<Vec<Grant>> {
    let id = decode_oid(ROLE_OID_PREFIX, role).ok_or_else(|| not_found("role", role))?;
    grants(pool, DbGrantee::Role(id), "list role grants").await
}

async fn grants(pool: &PgPool, grantee: DbGrantee, operation: &'static str) -> Result<Vec<Grant>> {
    let rows = list_grants(pool, grantee)
        .await
        .map_err(|error| principal_error(operation, error))?;
    rows.iter()
        .map(|row| grant_payload(row).map_err(|error| principal_error(operation, error)))
        .map(|payload| payload.map(grant_from_payload))
        .collect()
}

pub(crate) async fn create_grant(
    pool: &PgPool,
    grantee: &Grantee,
    secret: &GrantSecret,
) -> Result<Grant> {
    let operator = operator_id(pool).await?;
    let write = grant_write(grantee, secret);
    let (row, _) = db_create_grant(pool, &write, operator)
        .await
        .map_err(|error| principal_error("create grant", error))?;
    grant_payload(&row)
        .map(grant_from_payload)
        .map_err(|error| principal_error("create grant", error))
}

pub(crate) async fn upsert_static_secret(
    pool: &PgPool,
    input: &StaticSecretInput,
) -> Result<SecretRecord> {
    write_secret(
        pool,
        SecretKind::Static,
        WriteTarget::ForeignId(input.foreign_id.clone()),
        document(input, "static secret")?,
        "static secret",
    )
    .await
}

pub(crate) async fn upsert_oauth_token_secret(
    pool: &PgPool,
    input: &OAuthTokenSecretInput,
) -> Result<SecretRecord> {
    write_secret(
        pool,
        SecretKind::OauthToken,
        WriteTarget::ForeignId(input.foreign_id.clone()),
        deserialize_request::<OauthTokenRequest>(input, "oauth token secret")?.into(),
        "oauth token secret",
    )
    .await
}

pub(crate) async fn upsert_gcp_auth_secret(
    pool: &PgPool,
    input: &GcpAuthSecretInput,
) -> Result<SecretRecord> {
    let target = match &input.foreign_id {
        Some(foreign_id) => WriteTarget::ForeignId(foreign_id.clone()),
        None => WriteTarget::New,
    };
    write_secret(
        pool,
        SecretKind::GcpAuth,
        target,
        document(input, "GCP auth secret")?,
        "GCP auth secret",
    )
    .await
}

pub(crate) async fn upsert_gcp_id_token_secret(
    pool: &PgPool,
    input: &GcpIdTokenSecretInput,
) -> Result<SecretRecord> {
    write_secret(
        pool,
        SecretKind::GcpIdToken,
        WriteTarget::ForeignId(input.foreign_id.clone()),
        document(input, "GCP ID token secret")?,
        "GCP ID token secret",
    )
    .await
}

pub(crate) async fn upsert_pg_dsn_secret(
    pool: &PgPool,
    input: &PgDsnSecretInput,
) -> Result<SecretRecord> {
    write_secret(
        pool,
        SecretKind::PgDsn,
        WriteTarget::ForeignId(input.foreign_id.clone()),
        document(input, "Postgres DSN secret")?,
        "Postgres DSN secret",
    )
    .await
}

pub(crate) async fn upsert_hmac_secret(
    pool: &PgPool,
    input: &HmacSecretInput,
) -> Result<SecretRecord> {
    write_secret(
        pool,
        SecretKind::Hmac,
        WriteTarget::ForeignId(input.foreign_id.clone()),
        deserialize_request::<HmacRequest>(input, "HMAC secret")?.into(),
        "HMAC secret",
    )
    .await
}

pub(crate) async fn upsert_aws_auth_secret(
    pool: &PgPool,
    input: &AwsAuthSecretInput,
) -> Result<SecretRecord> {
    write_secret(
        pool,
        SecretKind::AwsAuth,
        WriteTarget::ForeignId(input.foreign_id.clone()),
        deserialize_request::<AwsAuthRequest>(input, "AWS auth secret")?.into(),
        "AWS auth secret",
    )
    .await
}

async fn write_secret(
    pool: &PgPool,
    kind: SecretKind,
    target: WriteTarget,
    document: WriteDocument,
    operation: &'static str,
) -> Result<SecretRecord> {
    let operator = operator_id(pool).await?;
    let result = db_write_secret(
        pool,
        kind,
        target,
        document,
        Some(operator),
        WriteOptions::default(),
    )
    .await
    .map_err(|error| write_error(operation, error))?;
    Ok(secret_record(result.row))
}

pub(crate) async fn create_proxy(
    pool: &PgPool,
    name: &str,
    principal: &str,
    requester_principal: Option<String>,
    labels: &BTreeMap<String, String>,
) -> Result<Proxy> {
    if name.trim().is_empty() {
        return Err(invalid_data("proxy name can't be blank".to_owned()));
    }
    let principal_id = resolve_principal(pool, Some(principal))
        .await?
        .ok_or_else(|| not_found("principal", principal))?;
    let requester_principal_id = resolve_principal(pool, requester_principal.as_deref()).await?;
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| IronControlError::InProcess {
        operation: "create proxy",
        message: format!("could not generate a proxy token: {error}"),
    })?;
    let token = format!("{TOKEN_PREFIX}{}", hex::encode(bytes));
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let row = db_create_proxy(
        pool,
        name,
        &string_map_value(labels),
        &token_hash,
        Some(principal_id),
        requester_principal_id,
    )
    .await
    .map_err(IronControlError::ConsoleDb)?;
    proxy_from_row(row, Some(token))
}

pub(crate) async fn assign_proxy_principal(
    pool: &PgPool,
    id: &str,
    principal: &str,
    requester_principal: Option<&str>,
    labels: &BTreeMap<String, String>,
) -> Result<Proxy> {
    let proxy_id = decode_oid(PROXY_OID_PREFIX, id).ok_or_else(|| not_found("proxy", id))?;
    let principal_id = resolve_principal(pool, Some(principal))
        .await?
        .ok_or_else(|| not_found("principal", principal))?;
    let requester_principal_id = resolve_principal(pool, requester_principal).await?;
    let change = ProxyChange {
        name: None,
        labels: (!labels.is_empty()).then(|| string_map_value(labels)),
        principal_id: Some(Some(principal_id)),
        // Always re-bind: an omitted requester is an explicit clear on a turn
        // with no requester, matching the HTTP payload.
        requester_principal_id: Some(requester_principal_id),
    };
    let row = db_update_proxy(pool, proxy_id, &change)
        .await
        .map_err(IronControlError::ConsoleDb)?
        .ok_or_else(|| not_found("proxy", id))?;
    proxy_from_row(row, None)
}

pub(crate) async fn delete_proxy(pool: &PgPool, id: &str) -> Result<()> {
    let proxy_id = decode_oid(PROXY_OID_PREFIX, id).ok_or_else(|| not_found("proxy", id))?;
    admin_proxy(pool, proxy_id)
        .await
        .map_err(IronControlError::ConsoleDb)?
        .ok_or_else(|| not_found("proxy", id))?;
    db_delete_proxy(pool, proxy_id)
        .await
        .map_err(IronControlError::ConsoleDb)
}

async fn resolve_principal(pool: &PgPool, reference: Option<&str>) -> Result<Option<i64>> {
    let Some(reference) = reference.filter(|reference| !reference.trim().is_empty()) else {
        return Ok(None);
    };
    let id = decode_oid(PRINCIPAL_OID_PREFIX, reference)
        .ok_or_else(|| not_found("principal", reference))?;
    find_principal(pool, id)
        .await
        .map_err(IronControlError::ConsoleDb)?
        .map(|principal| principal.id)
        .map(Some)
        .ok_or_else(|| not_found("principal", reference))
}

fn principal_write(input: &PrincipalInput) -> PrincipalWrite {
    PrincipalWrite {
        foreign_id: Some(input.foreign_id.clone()),
        name: Some(Some(input.name.clone())),
        kind: input.kind.clone().map(Some),
        labels: (!input.labels.is_empty()).then(|| string_map(&input.labels)),
        slack_user_id: input.slack_user_id.clone().map(Some),
        slack_channel_id: input.slack_channel_id.clone().map(Some),
        slack_team_id: input.slack_team_id.clone().map(Some),
        slack_email: input.slack_email.clone().map(Some),
        ..PrincipalWrite::default()
    }
}

fn grant_write(grantee: &Grantee, secret: &GrantSecret) -> GrantWrite {
    let mut write = GrantWrite::default();
    match grantee {
        Grantee::Principal(id) => write.principal_id = Some(id.clone()),
        Grantee::Role(id) => write.role_id = Some(id.clone()),
    }
    let id = secret.oid().to_owned();
    match secret {
        GrantSecret::Static(_) => write.static_secret_id = Some(id),
        GrantSecret::GcpAuth(_) => write.gcp_auth_secret_id = Some(id),
        GrantSecret::GcpIdToken(_) => write.gcp_id_token_secret_id = Some(id),
        GrantSecret::OAuthToken(_) => write.oauth_token_secret_id = Some(id),
        GrantSecret::PgDsn(_) => write.pg_dsn_secret_id = Some(id),
        GrantSecret::Hmac(_) => write.hmac_secret_id = Some(id),
        GrantSecret::AwsAuth(_) => write.aws_auth_secret_id = Some(id),
        GrantSecret::SshKey(_) => write.ssh_key_secret_id = Some(id),
    }
    write
}

fn role_from_view(view: centaur_console_db::areas::principals::RoleView) -> Result<Role> {
    role_from_parts(
        view.row.id,
        view.row.foreign_id,
        view.row.name,
        view.row.labels,
    )
}

fn role_from_parts(
    id: i64,
    foreign_id: Option<String>,
    name: Option<String>,
    labels: Value,
) -> Result<Role> {
    Ok(Role {
        id: centaur_console_db::oid("role", id),
        foreign_id,
        name: name.unwrap_or_default(),
        labels: string_labels(labels)?,
    })
}

fn grant_from_payload(payload: centaur_console_db::areas::principals::GrantPayload) -> Grant {
    Grant {
        id: payload.id,
        principal_id: payload.principal_id,
        role_id: payload.role_id,
        static_secret_id: payload.static_secret_id,
        oauth_token_secret_id: payload.oauth_token_secret_id,
        gcp_auth_secret_id: payload.gcp_auth_secret_id,
        gcp_id_token_secret_id: payload.gcp_id_token_secret_id,
        pg_dsn_secret_id: payload.pg_dsn_secret_id,
        hmac_secret_id: payload.hmac_secret_id,
        aws_auth_secret_id: payload.aws_auth_secret_id,
        ssh_key_secret_id: payload.ssh_key_secret_id,
    }
}

fn secret_record(row: SecretRow) -> SecretRecord {
    SecretRecord {
        id: row.oid(),
        foreign_id: row.foreign_id,
        name: row.name,
    }
}

fn proxy_from_row(row: ProxyRow, token: Option<String>) -> Result<Proxy> {
    Ok(Proxy {
        id: centaur_console_db::oid("prx", row.id),
        name: row.name,
        principal_id: row
            .principal_id
            .map(|id| centaur_console_db::oid("prn", id))
            .unwrap_or_default(),
        labels: string_labels(row.labels)?,
        token,
    })
}

fn document(input: &impl Serialize, operation: &'static str) -> Result<WriteDocument> {
    let payload: SecretDocumentPayload = deserialize_request(input, operation)?;
    Ok(payload.into())
}

fn deserialize_request<T: DeserializeOwned>(
    input: &impl Serialize,
    operation: &'static str,
) -> Result<T> {
    let value = serde_json::to_value(input).map_err(|error| IronControlError::InProcess {
        operation,
        message: format!("could not serialize the request: {error}"),
    })?;
    serde_json::from_value(value).map_err(|error| IronControlError::InProcess {
        operation,
        message: format!("request did not match Console's document: {error}"),
    })
}

fn decode_oid(prefix: &str, oid: &str) -> Option<i64> {
    let prefix = prefix.trim_end_matches('_');
    centaur_console_db::decode_oid(prefix, oid)
}

fn not_found(resource: &'static str, ident: &str) -> IronControlError {
    IronControlError::NotFound {
        resource,
        ident: ident.to_owned(),
    }
}

fn invalid_data(message: String) -> IronControlError {
    IronControlError::ConsoleDb(centaur_console_db::Error::InvalidData(message))
}

fn principal_error(operation: &'static str, error: PrincipalError) -> IronControlError {
    match error {
        PrincipalError::NotFound(message) => IronControlError::InProcess { operation, message },
        PrincipalError::Database(error) => IronControlError::ConsoleDb(error.into()),
        PrincipalError::Encryption(error) => {
            IronControlError::ConsoleDb(centaur_console_db::Error::Encryption(error))
        }
        PrincipalError::BadRequest(message) | PrincipalError::Internal(message) => {
            IronControlError::InProcess { operation, message }
        }
        PrincipalError::Validation(errors) => IronControlError::InProcess {
            operation,
            message: errors.to_string(),
        },
    }
}

fn write_error(
    operation: &'static str,
    error: centaur_console_db::areas::secrets::WriteError,
) -> IronControlError {
    use centaur_console_db::areas::secrets::WriteError;
    match error {
        WriteError::NotFound(message) => IronControlError::InProcess { operation, message },
        WriteError::Validation(details) => IronControlError::InProcess {
            operation,
            message: format!("validation failed: {details}"),
        },
        WriteError::BadRequest(message) => IronControlError::InProcess { operation, message },
        WriteError::Internal(error) => IronControlError::ConsoleDb(error),
    }
}

fn string_map(labels: &BTreeMap<String, String>) -> Map<String, Value> {
    labels
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect()
}

fn string_map_value(labels: &BTreeMap<String, String>) -> Value {
    Value::Object(string_map(labels))
}

fn string_labels(value: Value) -> Result<BTreeMap<String, String>> {
    let Value::Object(stored) = value else {
        return Err(invalid_data("labels are not an object".to_owned()));
    };
    let mut labels = BTreeMap::new();
    for (key, value) in stored {
        let Value::String(value) = value else {
            return Err(invalid_data(format!("label {key:?} is not a string")));
        };
        labels.insert(key, value);
    }
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oids_decode_with_the_console_prefix() {
        let oid = centaur_console_db::oid("prn", 42);
        assert_eq!(decode_oid(PRINCIPAL_OID_PREFIX, &oid), Some(42));
        assert_eq!(decode_oid(PRINCIPAL_OID_PREFIX, "not-an-oid"), None);
    }
}
