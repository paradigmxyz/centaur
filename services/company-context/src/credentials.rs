use std::{str::FromStr, sync::Arc};

use active_record_encryption::ActiveRecordEncryption;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};

use crate::config::Config;

#[derive(Clone)]
pub struct ConsoleCredentials {
    pool: PgPool,
    encryption: Arc<ActiveRecordEncryption>,
    google_foreign_id: String,
    embeddings_foreign_id: String,
}

impl ConsoleCredentials {
    pub async fn connect(config: &Config) -> Result<Self> {
        let mut options = PgConnectOptions::from_str(&config.console_database_url)
            .context("parse IRON_CONTROL_DATABASE_URL")?;
        if let Some(database_name) = &config.console_database_name {
            options = options.database(database_name);
        }
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .context("connect to Rails Console database")?;
        let credentials = Self {
            pool,
            encryption: Arc::new(ActiveRecordEncryption::new(
                &config.active_record_primary_key,
                &config.active_record_key_derivation_salt,
            )),
            google_foreign_id: config.google_credential_foreign_id.clone(),
            embeddings_foreign_id: config.embeddings_credential_foreign_id.clone(),
        };
        credentials.google_access_token().await?;
        credentials.embeddings_api_key().await?;
        Ok(credentials)
    }

    pub async fn google_access_token(&self) -> Result<String> {
        let row = sqlx::query(
            "SELECT access_token, expires_at, dead FROM broker_credentials WHERE foreign_id = $1",
        )
        .bind(&self.google_foreign_id)
        .fetch_optional(&self.pool)
        .await
        .context("load Google broker credential from Rails Console")?
        .with_context(|| {
            format!(
                "Rails Console broker credential {:?} was not found",
                self.google_foreign_id
            )
        })?;
        ensure_live_broker(
            row.try_get("dead")?,
            row.try_get("expires_at")?,
            &self.google_foreign_id,
        )?;
        self.decrypt_required(
            row.try_get("access_token")?,
            "Google broker credential access token",
        )
    }

    pub async fn embeddings_api_key(&self) -> Result<String> {
        let row = sqlx::query(
            "SELECT sources.source_type, sources.secret, broker.access_token, \
                    broker.dead, broker.expires_at \
             FROM static_secrets credentials \
             JOIN secret_sources sources ON sources.static_secret_id = credentials.id \
             LEFT JOIN broker_credentials broker ON broker.id = sources.broker_credential_id \
             WHERE credentials.foreign_id = $1",
        )
        .bind(&self.embeddings_foreign_id)
        .fetch_optional(&self.pool)
        .await
        .context("load embeddings credential from Rails Console")?
        .with_context(|| {
            format!(
                "Rails Console static credential {:?} was not found",
                self.embeddings_foreign_id
            )
        })?;
        let source_type: String = row.try_get("source_type")?;
        match source_type.as_str() {
            "control_plane" => self.decrypt_required(
                row.try_get("secret")?,
                "embeddings control-plane credential",
            ),
            "token_broker" => {
                ensure_live_broker(
                    row.try_get("dead")?,
                    row.try_get("expires_at")?,
                    &self.embeddings_foreign_id,
                )?;
                self.decrypt_required(
                    row.try_get("access_token")?,
                    "embeddings broker credential access token",
                )
            }
            _ => bail!(
                "Rails Console embeddings credential {:?} uses unsupported source type {:?}; expected control_plane or token_broker",
                self.embeddings_foreign_id,
                source_type
            ),
        }
    }

    pub async fn ready(&self) -> bool {
        self.google_access_token().await.is_ok() && self.embeddings_api_key().await.is_ok()
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    fn decrypt_required(&self, encrypted: Option<String>, description: &str) -> Result<String> {
        let encrypted = encrypted.with_context(|| format!("{description} is absent"))?;
        let value = self
            .encryption
            .decrypt(&encrypted)
            .with_context(|| format!("decrypt {description}"))?;
        if value.trim().is_empty() {
            bail!("{description} is empty");
        }
        Ok(value)
    }
}

fn ensure_live_broker(
    dead: bool,
    expires_at: Option<DateTime<Utc>>,
    credential_name: &str,
) -> Result<()> {
    if dead {
        bail!("Rails Console broker credential {credential_name:?} is marked dead");
    }
    if expires_at.is_some_and(|expires_at| expires_at <= Utc::now()) {
        bail!("Rails Console broker credential {credential_name:?} is expired");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    #[test]
    fn rejects_dead_and_expired_broker_credentials() {
        assert!(ensure_live_broker(true, None, "google").is_err());
        assert!(
            ensure_live_broker(false, Some(Utc::now() - Duration::seconds(1)), "google").is_err()
        );
        assert!(
            ensure_live_broker(false, Some(Utc::now() + Duration::seconds(60)), "google").is_ok()
        );
    }
}
