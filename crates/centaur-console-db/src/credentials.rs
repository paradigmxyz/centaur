use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::{PgPool, Row};

use crate::{
    Error, Result,
    models::{Credential, CredentialData, CredentialKind},
};

const CREDENTIALS_SQL: &str = include_str!("../sql/effective_credentials.sql");

/// The Slack channels a principal may upload to, download from, and read the
/// history of, through direct permissions and its roles' permissions.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SlackChannels {
    pub upload: Vec<String>,
    pub download: Vec<String>,
    pub history: Vec<String>,
}

/// Every credential granted to `principal_id`, plus the always-available
/// OAuth wrappers granted directly to `requester_id`, each with its strongest
/// grant priority. Ordered by kind, then ascending priority, then id.
pub async fn load_credentials(
    pool: &PgPool,
    principal_id: i64,
    requester_id: Option<i64>,
) -> Result<Vec<Credential>> {
    let rows = sqlx::query(CREDENTIALS_SQL)
        .bind(principal_id)
        .bind(requester_id)
        .fetch_all(pool)
        .await?;
    rows.into_iter()
        .map(|row| {
            let id = row.try_get("credential_id")?;
            let kind_name: String = row.try_get("kind")?;
            let kind = CredentialKind::parse(&kind_name).ok_or_else(|| {
                Error::InvalidData(format!("credential {id} has unknown kind {kind_name:?}"))
            })?;
            let raw_data = row.try_get("credential")?;
            Ok(Credential {
                kind,
                id,
                priority: row.try_get("effective_priority")?,
                data: credential_data(kind, id, raw_data)?,
                sources: deserialize_json(id, "sources", row.try_get("sources")?)?,
                rules: deserialize_json(id, "rules", row.try_get("rules")?)?,
            })
        })
        .collect()
}

pub async fn slack_channels(pool: &PgPool, principal_id: i64) -> Result<SlackChannels> {
    let rows = sqlx::query(
        "SELECT channel_id, bool_or(upload_enabled) AS upload, bool_or(download_enabled) AS download, bool_or(history_enabled) AS history \
         FROM slack_channel_permissions \
         WHERE principal_id = $1 OR role_id IN (SELECT role_id FROM principal_roles WHERE principal_id = $1) \
         GROUP BY channel_id ORDER BY channel_id",
    )
    .bind(principal_id)
    .fetch_all(pool)
    .await?;
    let mut channels = SlackChannels::default();
    for row in rows {
        let channel: String = row.try_get("channel_id")?;
        if row.try_get("upload")? {
            channels.upload.push(channel.clone());
        }
        if row.try_get("download")? {
            channels.download.push(channel.clone());
        }
        if row.try_get("history")? {
            channels.history.push(channel);
        }
    }
    Ok(channels)
}

fn credential_data(kind: CredentialKind, id: i64, value: Value) -> Result<CredentialData> {
    Ok(match kind {
        CredentialKind::Static => CredentialData::Static(deserialize_json(id, "data", value)?),
        CredentialKind::GcpAuth => CredentialData::GcpAuth(deserialize_json(id, "data", value)?),
        CredentialKind::GcpIdToken => {
            CredentialData::GcpIdToken(deserialize_json(id, "data", value)?)
        }
        CredentialKind::AwsAuth => CredentialData::AwsAuth(deserialize_json(id, "data", value)?),
        CredentialKind::OauthToken => {
            CredentialData::OauthToken(deserialize_json(id, "data", value)?)
        }
        CredentialKind::PgDsn => CredentialData::PgDsn(deserialize_json(id, "data", value)?),
        CredentialKind::Hmac => CredentialData::Hmac(deserialize_json(id, "data", value)?),
    })
}

fn deserialize_json<T: DeserializeOwned>(
    credential_id: i64,
    field: &str,
    value: Value,
) -> Result<T> {
    serde_json::from_value(value).map_err(|error| {
        Error::InvalidData(format!(
            "credential {credential_id} has invalid {field}: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::credential_data;
    use crate::models::CredentialKind;

    #[test]
    fn malformed_required_credential_fields_are_rejected() {
        let error = credential_data(CredentialKind::GcpIdToken, 42, json!({}))
            .expect_err("audience is required");
        let message = error.to_string();

        assert!(message.contains("credential 42 has invalid data"));
        assert!(message.contains("missing field `audience`"));
    }
}
