use serde_json::Value;
use sqlx::{PgPool, Row};

use crate::{Error, Result};

/// A machine client of the api-rs session API, as Console stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiClient {
    pub id: i64,
    pub name: String,
    /// Lowercase hex SHA-256 of the client secret.
    pub secret_digest: String,
    pub scopes: Vec<String>,
    pub disabled: bool,
}

/// A principal attached to an API client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiClientPrincipal {
    pub principal_id: i64,
    pub foreign_id: Option<String>,
    pub default: bool,
}

pub async fn find_api_client(pool: &PgPool, id: i64) -> Result<Option<ApiClient>> {
    let Some(row) = sqlx::query(
        "SELECT id, name, secret_digest, scopes, \
                disabled_at IS NOT NULL AS disabled \
         FROM api_clients WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let scopes: Value = row.try_get("scopes")?;
    let scopes = scopes
        .as_array()
        .and_then(|scopes| {
            scopes
                .iter()
                .map(|scope| scope.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| Error::InvalidData(format!("api client {id} scopes are not strings")))?;
    Ok(Some(ApiClient {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        secret_digest: row.try_get("secret_digest")?,
        scopes,
        disabled: row.try_get("disabled")?,
    }))
}

/// The principals `api_client_id` may run sessions as.
pub async fn api_client_principals(
    pool: &PgPool,
    api_client_id: i64,
) -> Result<Vec<ApiClientPrincipal>> {
    let rows = sqlx::query(
        "SELECT p.id, p.foreign_id, l.\"default\" \
         FROM api_client_principals l JOIN principals p ON p.id = l.principal_id \
         WHERE l.api_client_id = $1 ORDER BY l.id",
    )
    .bind(api_client_id)
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(ApiClientPrincipal {
                principal_id: row.try_get("id")?,
                foreign_id: row.try_get("foreign_id")?,
                default: row.try_get("default")?,
            })
        })
        .collect()
}
