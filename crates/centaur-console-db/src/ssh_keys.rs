//! SSH keys granted to a principal. Unlike other credentials they never enter
//! the synced proxy configuration: the proxy's SSH authorizer releases one key
//! per connection, and only for a destination the deployment allows.

use serde_json::Value;
use sqlx::{PgPool, Row};

use crate::{
    Result,
    config::{SecretValues, source_value},
    models::SecretSource,
};

/// One SSH key a principal may use, with its strongest grant priority.
#[derive(Debug)]
pub struct SshKey {
    pub id: i64,
    pub priority: i32,
    /// Changes whenever the key or its source is replaced.
    pub version: String,
    /// The private key as an iron-proxy secret source.
    pub private_key: Value,
}

const SSH_KEYS_SQL: &str = "\
WITH effective_grants AS (
    SELECT g.ssh_key_secret_id, g.priority
    FROM grants AS g
    WHERE g.principal_id = $1 AND g.ssh_key_secret_id IS NOT NULL
    UNION ALL
    SELECT g.ssh_key_secret_id, g.priority
    FROM principal_roles AS pr
    JOIN grants AS g ON g.role_id = pr.role_id
    WHERE pr.principal_id = $1 AND g.ssh_key_secret_id IS NOT NULL
),
granted AS (
    SELECT ssh_key_secret_id, MAX(priority) AS priority
    FROM effective_grants
    GROUP BY ssh_key_secret_id
)
SELECT
    k.id,
    granted.priority,
    k.updated_at::text AS key_updated_at,
    s.updated_at::text AS source_updated_at,
    to_jsonb(s) AS source
FROM granted
JOIN ssh_key_secrets AS k ON k.id = granted.ssh_key_secret_id
JOIN secret_sources AS s ON s.ssh_key_secret_id = k.id
LEFT JOIN users AS owner ON owner.id = k.owner_id
WHERE k.owner_id IS NULL OR owner.status = 'active'
ORDER BY granted.priority DESC, k.id";

/// Every SSH key granted to `principal_id`, directly or through its roles,
/// strongest grant first. A personal key whose owner is no longer active is
/// omitted, as is a key whose source cannot currently deliver a value.
pub async fn ssh_keys(
    pool: &PgPool,
    values: SecretValues<'_>,
    principal_id: i64,
) -> Result<Vec<SshKey>> {
    let rows = sqlx::query(SSH_KEYS_SQL)
        .bind(principal_id)
        .fetch_all(pool)
        .await?;
    let mut keys = Vec::with_capacity(rows.len());
    for row in rows {
        let id: i64 = row.try_get("id")?;
        let source: SecretSource =
            serde_json::from_value(row.try_get("source")?).map_err(|error| {
                crate::Error::InvalidData(format!("SSH key {id} has an invalid source: {error}"))
            })?;
        let Some(private_key) = source_value(&source, values)? else {
            continue;
        };
        let key_updated_at: String = row.try_get("key_updated_at")?;
        let source_updated_at: String = row.try_get("source_updated_at")?;
        keys.push(SshKey {
            id,
            priority: row.try_get("priority")?,
            version: format!("{key_updated_at}/{source_updated_at}"),
            private_key,
        });
    }
    Ok(keys)
}
