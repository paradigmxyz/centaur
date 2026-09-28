use std::{str::FromStr, sync::Arc};

use active_record_encryption::ActiveRecordEncryption;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, Utc};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
    types::Json,
};
use tokio::sync::Mutex;

use crate::config::Config;

const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_TOKEN_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";
const TOKEN_REFRESH_MARGIN: Duration = Duration::minutes(5);

#[derive(Clone)]
pub struct ConsoleCredentials {
    pool: PgPool,
    encryption: Arc<ActiveRecordEncryption>,
    http: Client,
    google_foreign_id: String,
    google_token: Arc<Mutex<Option<CachedToken>>>,
}

struct CachedToken {
    value: String,
    expires_at: DateTime<Utc>,
}

struct GoogleCredential {
    client_email: String,
    private_key: String,
    scopes: Vec<String>,
    subject: Option<String>,
}

#[derive(Deserialize)]
struct ServiceAccountKeyfile {
    client_email: String,
    private_key: String,
}

#[derive(Serialize)]
struct GoogleJwtClaims<'a> {
    iss: &'a str,
    scope: String,
    aud: &'static str,
    iat: i64,
    exp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    sub: Option<&'a str>,
}

#[derive(Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    expires_in: i64,
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
            http: Client::new(),
            google_foreign_id: config.google_credential_foreign_id.clone(),
            google_token: Arc::new(Mutex::new(None)),
        };
        credentials.load_google_credential().await?;
        Ok(credentials)
    }

    pub async fn google_access_token(&self) -> Result<String> {
        let mut cached = self.google_token.lock().await;
        let now = Utc::now();
        if let Some(token) = cached
            .as_ref()
            .filter(|token| token.expires_at - TOKEN_REFRESH_MARGIN > now)
        {
            return Ok(token.value.clone());
        }

        let credential = self.load_google_credential().await?;
        let claims = google_jwt_claims(&credential, now);
        let assertion = encode(
            &Header::new(Algorithm::RS256),
            &claims,
            &EncodingKey::from_rsa_pem(credential.private_key.as_bytes())
                .context("parse Google service-account private key")?,
        )
        .context("sign Google service-account assertion")?;
        let response = self
            .http
            .post(GOOGLE_TOKEN_ENDPOINT)
            .form(&[
                ("grant_type", GOOGLE_TOKEN_GRANT),
                ("assertion", &assertion),
            ])
            .send()
            .await
            .context("request Google service-account access token")?;
        let status = response.status();
        if !status.is_success() {
            bail!("Google service-account token endpoint returned {status}");
        }
        let token: GoogleTokenResponse = response
            .json()
            .await
            .context("decode Google service-account token response")?;
        if token.access_token.trim().is_empty() || token.expires_in <= 0 {
            bail!("Google service-account token response was invalid");
        }
        let value = token.access_token;
        *cached = Some(CachedToken {
            value: value.clone(),
            expires_at: now + Duration::seconds(token.expires_in),
        });
        Ok(value)
    }

    async fn load_google_credential(&self) -> Result<GoogleCredential> {
        let row = sqlx::query(
            r#"
            SELECT credentials.scopes,
                   credentials.subject,
                   credentials.credentials_provider,
                   sources.source_type,
                   sources.secret
            FROM gcp_auth_secrets credentials
            LEFT JOIN secret_sources sources
              ON sources.gcp_auth_secret_id = credentials.id
            WHERE credentials.foreign_id = $1
            "#,
        )
        .bind(&self.google_foreign_id)
        .fetch_optional(&self.pool)
        .await
        .context("load Google credential from Rails Console")?
        .with_context(|| {
            format!(
                "Rails Console GCP auth credential {:?} was not found",
                self.google_foreign_id
            )
        })?;
        let provider: Option<Value> = row.try_get("credentials_provider")?;
        if provider.is_some() {
            bail!(
                "Rails Console GCP auth credential {:?} uses a credentials provider; a control_plane keyfile source is required",
                self.google_foreign_id
            );
        }
        let source_type: Option<String> = row.try_get("source_type")?;
        if source_type.as_deref() != Some("control_plane") {
            bail!(
                "Rails Console GCP auth credential {:?} must use a control_plane keyfile source",
                self.google_foreign_id
            );
        }
        let keyfile: ServiceAccountKeyfile = serde_json::from_str(
            &self.decrypt_required(row.try_get("secret")?, "Google service-account keyfile")?,
        )
        .context("decode Google service-account keyfile")?;
        let Json(scopes): Json<Vec<String>> = row.try_get("scopes")?;
        if scopes.is_empty() {
            bail!("Google credential has no OAuth scopes");
        }
        Ok(GoogleCredential {
            client_email: keyfile.client_email,
            private_key: keyfile.private_key,
            scopes,
            subject: row.try_get("subject")?,
        })
    }

    pub async fn ready(&self) -> bool {
        self.google_access_token().await.is_ok()
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

fn google_jwt_claims(credential: &GoogleCredential, now: DateTime<Utc>) -> GoogleJwtClaims<'_> {
    GoogleJwtClaims {
        iss: &credential.client_email,
        scope: credential.scopes.join(" "),
        aud: GOOGLE_TOKEN_ENDPOINT,
        iat: now.timestamp(),
        exp: (now + Duration::hours(1)).timestamp(),
        sub: credential.subject.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_claims_include_scopes_and_delegated_subject() {
        let now = Utc::now();
        let credential = GoogleCredential {
            client_email: "reader@example.invalid".to_owned(),
            private_key: "unused".to_owned(),
            scopes: vec!["scope-a".to_owned(), "scope-b".to_owned()],
            subject: Some("user@example.invalid".to_owned()),
        };
        let claims = google_jwt_claims(&credential, now);
        assert_eq!(claims.iss, "reader@example.invalid");
        assert_eq!(claims.scope, "scope-a scope-b");
        assert_eq!(claims.sub, Some("user@example.invalid"));
        assert_eq!(claims.exp - claims.iat, 3_600);
    }
}
