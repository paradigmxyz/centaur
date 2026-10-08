use std::{str::FromStr, sync::Arc};

use active_record_encryption::ActiveRecordEncryption;
use anyhow::{Context, Result, bail};
use chrono::{NaiveDateTime, Utc};
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
    types::Json,
};

use crate::{config::Config, slack::conversation_types};

const DRIVE_READONLY_SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";

#[derive(Clone)]
pub struct ConsoleCredentials {
    pool: PgPool,
    encryption: Arc<ActiveRecordEncryption>,
    google_oauth_app_slug: String,
    granola_oauth_app_slug: String,
    slack_oauth_app_slug: String,
    /// Sync limits; an empty list allows every credential.
    google_user_emails: Vec<String>,
    granola_user_emails: Vec<String>,
    slack_user_ids: Vec<String>,
    slack_conversation_types: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct SlackCredential {
    pub id: i64,
    pub access_token: String,
    /// Conversation types the credential's scopes can list and read.
    pub conversation_types: Vec<&'static str>,
    /// Whether the credential's scopes can download files.
    pub can_read_files: bool,
}

#[derive(Clone, Debug)]
pub struct GranolaCredential {
    pub id: i64,
    pub access_token: String,
    pub provider_email: String,
    pub provider_subject: String,
}

#[derive(Clone, Debug)]
pub struct GoogleCredential {
    pub id: i64,
    pub access_token: String,
    pub provider_email: String,
    pub provider_subject: String,
    pub revision: String,
}

/// The provider identities a Console principal is known by. Google and
/// Granola identities are the subjects of the live broker credentials granted
/// directly to the principal, the accounts it can already use through the
/// proxy; principal labels are not trusted.
#[derive(Clone, Debug, Default, sqlx::FromRow)]
pub struct PrincipalIdentity {
    pub slack_user_id: Option<String>,
    pub google_subjects: Vec<String>,
    pub granola_subjects: Vec<String>,
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
            google_oauth_app_slug: config.google_oauth_app_slug.clone(),
            granola_oauth_app_slug: config.granola_oauth_app_slug.clone(),
            slack_oauth_app_slug: config.slack_oauth_app_slug.clone(),
            google_user_emails: config.google_drive_user_emails.clone(),
            granola_user_emails: config.granola_user_emails.clone(),
            slack_user_ids: config.slack_user_ids.clone(),
            slack_conversation_types: config.slack_conversation_types.clone(),
        };
        credentials.google_credential_ids().await?;
        Ok(credentials)
    }

    pub async fn google_credential_ids(&self) -> Result<Vec<i64>> {
        let rows = sqlx::query(
            r#"
            SELECT credentials.id, credentials.scopes
            FROM broker_credentials credentials
            JOIN oauth_apps app ON app.id = credentials.oauth_app_id
            WHERE app.provider = 'google'
              AND app.slug = $1
              AND app.enabled = TRUE
              AND credentials.dead = FALSE
              AND (
                  cardinality($2::text[]) = 0
                  OR LOWER(credentials.provider_email) = ANY($2::text[])
              )
              AND credentials.access_token IS NOT NULL
              AND (
                  credentials.expires_at IS NULL
                  OR credentials.expires_at > NOW()
              )
            ORDER BY credentials.id
            "#,
        )
        .bind(&self.google_oauth_app_slug)
        .bind(&self.google_user_emails)
        .fetch_all(&self.pool)
        .await
        .context("list Google broker credentials from Rails Console")?;

        let mut ids = Vec::new();
        for row in rows {
            let Json(scopes): Json<Vec<String>> = row
                .try_get("scopes")
                .context("decode Google broker credential scopes")?;
            if scopes.iter().any(|scope| scope == DRIVE_READONLY_SCOPE) {
                ids.push(
                    row.try_get("id")
                        .context("decode Google broker credential ID")?,
                );
            }
        }
        Ok(ids)
    }

    pub async fn retained_google_credential_ids(&self) -> Result<Vec<i64>> {
        sqlx::query_scalar(
            r#"
            SELECT credentials.id
            FROM broker_credentials credentials
            JOIN oauth_apps app ON app.id = credentials.oauth_app_id
            WHERE app.provider = 'google'
              AND app.slug = $1
              AND credentials.dead = FALSE
              AND (
                  cardinality($2::text[]) = 0
                  OR LOWER(credentials.provider_email) = ANY($2::text[])
              )
            ORDER BY credentials.id
            "#,
        )
        .bind(&self.google_oauth_app_slug)
        .bind(&self.google_user_emails)
        .fetch_all(&self.pool)
        .await
        .context("list retained Google broker credentials from Rails Console")
    }

    pub async fn google_credential(&self, credential_id: i64) -> Result<GoogleCredential> {
        let row = sqlx::query(
            r#"
            SELECT credentials.id,
                   credentials.access_token,
                   credentials.expires_at,
                   credentials.scopes,
                   credentials.provider_email,
                   credentials.provider_subject,
                   credentials.updated_at
            FROM broker_credentials credentials
            JOIN oauth_apps app ON app.id = credentials.oauth_app_id
            WHERE credentials.id = $1
              AND app.provider = 'google'
              AND app.slug = $2
              AND app.enabled = TRUE
              AND credentials.dead = FALSE
              AND (
                  cardinality($3::text[]) = 0
                  OR LOWER(credentials.provider_email) = ANY($3::text[])
              )
            "#,
        )
        .bind(credential_id)
        .bind(&self.google_oauth_app_slug)
        .bind(&self.google_user_emails)
        .fetch_optional(&self.pool)
        .await
        .context("load Google broker credential from Rails Console")?
        .with_context(|| format!("Google broker credential {credential_id} is not syncable"))?;

        let expires_at: Option<NaiveDateTime> = row.try_get("expires_at")?;
        if expires_at.is_some_and(|expires_at| expires_at <= Utc::now().naive_utc()) {
            bail!("Google broker credential {credential_id} is expired");
        }
        let Json(scopes): Json<Vec<String>> = row.try_get("scopes")?;
        if !scopes.iter().any(|scope| scope == DRIVE_READONLY_SCOPE) {
            bail!("Google broker credential {credential_id} lacks Drive read access");
        }
        Ok(GoogleCredential {
            id: credential_id,
            access_token: self
                .decrypt_required(row.try_get("access_token")?, "Google broker access token")?,
            provider_email: row
                .try_get::<Option<String>, _>("provider_email")?
                .unwrap_or_default(),
            provider_subject: row
                .try_get::<Option<String>, _>("provider_subject")?
                .unwrap_or_default(),
            revision: row
                .try_get::<NaiveDateTime, _>("updated_at")?
                .and_utc()
                .to_rfc3339(),
        })
    }

    pub async fn granola_credential_ids(&self) -> Result<Vec<i64>> {
        sqlx::query_scalar(
            r#"
            SELECT credentials.id
            FROM broker_credentials credentials
            JOIN oauth_apps app ON app.id = credentials.oauth_app_id
            WHERE app.provider = 'granola'
              AND app.slug = $1
              AND app.enabled = TRUE
              AND credentials.dead = FALSE
              AND (
                  cardinality($2::text[]) = 0
                  OR LOWER(credentials.provider_email) = ANY($2::text[])
              )
              AND credentials.access_token IS NOT NULL
              AND (
                  credentials.expires_at IS NULL
                  OR credentials.expires_at > NOW()
              )
            ORDER BY credentials.id
            "#,
        )
        .bind(&self.granola_oauth_app_slug)
        .bind(&self.granola_user_emails)
        .fetch_all(&self.pool)
        .await
        .context("list Granola broker credentials from Rails Console")
    }

    pub async fn retained_granola_credential_ids(&self) -> Result<Vec<i64>> {
        sqlx::query_scalar(
            r#"
            SELECT credentials.id
            FROM broker_credentials credentials
            JOIN oauth_apps app ON app.id = credentials.oauth_app_id
            WHERE app.provider = 'granola'
              AND app.slug = $1
              AND credentials.dead = FALSE
              AND (
                  cardinality($2::text[]) = 0
                  OR LOWER(credentials.provider_email) = ANY($2::text[])
              )
            ORDER BY credentials.id
            "#,
        )
        .bind(&self.granola_oauth_app_slug)
        .bind(&self.granola_user_emails)
        .fetch_all(&self.pool)
        .await
        .context("list retained Granola broker credentials from Rails Console")
    }

    pub async fn granola_credential(&self, credential_id: i64) -> Result<GranolaCredential> {
        let row = sqlx::query(
            r#"
            SELECT credentials.access_token,
                   credentials.expires_at,
                   credentials.provider_email,
                   credentials.provider_subject
            FROM broker_credentials credentials
            JOIN oauth_apps app ON app.id = credentials.oauth_app_id
            WHERE credentials.id = $1
              AND app.provider = 'granola'
              AND app.slug = $2
              AND app.enabled = TRUE
              AND credentials.dead = FALSE
              AND (
                  cardinality($3::text[]) = 0
                  OR LOWER(credentials.provider_email) = ANY($3::text[])
              )
            "#,
        )
        .bind(credential_id)
        .bind(&self.granola_oauth_app_slug)
        .bind(&self.granola_user_emails)
        .fetch_optional(&self.pool)
        .await
        .context("load Granola broker credential from Rails Console")?
        .with_context(|| format!("Granola broker credential {credential_id} is not syncable"))?;

        let expires_at: Option<NaiveDateTime> = row.try_get("expires_at")?;
        if expires_at.is_some_and(|expires_at| expires_at <= Utc::now().naive_utc()) {
            bail!("Granola broker credential {credential_id} is expired");
        }
        Ok(GranolaCredential {
            id: credential_id,
            access_token: self
                .decrypt_required(row.try_get("access_token")?, "Granola broker access token")?,
            provider_email: row
                .try_get::<Option<String>, _>("provider_email")?
                .unwrap_or_default(),
            provider_subject: row
                .try_get::<Option<String>, _>("provider_subject")?
                .unwrap_or_default(),
        })
    }

    /// Lists the credentials the Console Slack DM sync selects, narrowed to
    /// those whose scopes cover an ingested conversation type.
    pub async fn slack_credential_ids(&self) -> Result<Vec<i64>> {
        let rows = sqlx::query(
            r#"
            SELECT credentials.id, credentials.scopes
            FROM broker_credentials credentials
            JOIN oauth_apps app ON app.id = credentials.oauth_app_id
            WHERE app.provider = 'slack'
              AND app.slug = $1
              AND app.enabled = TRUE
              AND credentials.dead = FALSE
              AND (
                  cardinality($2::text[]) = 0
                  OR credentials.provider_subject = ANY($2::text[])
              )
              AND credentials.access_token IS NOT NULL
              AND (
                  credentials.expires_at IS NULL
                  OR credentials.expires_at > NOW()
              )
            ORDER BY credentials.id
            "#,
        )
        .bind(&self.slack_oauth_app_slug)
        .bind(&self.slack_user_ids)
        .fetch_all(&self.pool)
        .await
        .context("list Slack broker credentials from Rails Console")?;

        let mut ids = Vec::new();
        for row in rows {
            let Json(scopes): Json<Vec<String>> = row
                .try_get("scopes")
                .context("decode Slack broker credential scopes")?;
            if !conversation_types(&scopes, &self.slack_conversation_types).is_empty() {
                ids.push(
                    row.try_get("id")
                        .context("decode Slack broker credential ID")?,
                );
            }
        }
        Ok(ids)
    }

    pub async fn retained_slack_credential_ids(&self) -> Result<Vec<i64>> {
        sqlx::query_scalar(
            r#"
            SELECT credentials.id
            FROM broker_credentials credentials
            JOIN oauth_apps app ON app.id = credentials.oauth_app_id
            WHERE app.provider = 'slack'
              AND app.slug = $1
              AND credentials.dead = FALSE
              AND (
                  cardinality($2::text[]) = 0
                  OR credentials.provider_subject = ANY($2::text[])
              )
            ORDER BY credentials.id
            "#,
        )
        .bind(&self.slack_oauth_app_slug)
        .bind(&self.slack_user_ids)
        .fetch_all(&self.pool)
        .await
        .context("list retained Slack broker credentials from Rails Console")
    }

    pub async fn slack_credential(&self, credential_id: i64) -> Result<SlackCredential> {
        let row = sqlx::query(
            r#"
            SELECT credentials.access_token,
                   credentials.expires_at,
                   credentials.scopes
            FROM broker_credentials credentials
            JOIN oauth_apps app ON app.id = credentials.oauth_app_id
            WHERE credentials.id = $1
              AND app.provider = 'slack'
              AND app.slug = $2
              AND app.enabled = TRUE
              AND credentials.dead = FALSE
              AND (
                  cardinality($3::text[]) = 0
                  OR credentials.provider_subject = ANY($3::text[])
              )
            "#,
        )
        .bind(credential_id)
        .bind(&self.slack_oauth_app_slug)
        .bind(&self.slack_user_ids)
        .fetch_optional(&self.pool)
        .await
        .context("load Slack broker credential from Rails Console")?
        .with_context(|| format!("Slack broker credential {credential_id} is not syncable"))?;

        let expires_at: Option<NaiveDateTime> = row.try_get("expires_at")?;
        if expires_at.is_some_and(|expires_at| expires_at <= Utc::now().naive_utc()) {
            bail!("Slack broker credential {credential_id} is expired");
        }
        let Json(scopes): Json<Vec<String>> = row.try_get("scopes")?;
        Ok(SlackCredential {
            id: credential_id,
            access_token: self
                .decrypt_required(row.try_get("access_token")?, "Slack broker access token")?,
            conversation_types: conversation_types(&scopes, &self.slack_conversation_types),
            can_read_files: scopes.iter().any(|scope| scope == "files:read"),
        })
    }

    pub async fn principal_identity(&self, principal_id: i64) -> Result<Option<PrincipalIdentity>> {
        sqlx::query_as(
            r#"
            WITH granted AS (
                SELECT app.provider, app.slug, credentials.provider_subject
                FROM grants
                JOIN static_secrets secrets ON secrets.id = grants.static_secret_id
                JOIN broker_credentials credentials
                  ON credentials.id = secrets.broker_credential_id
                JOIN oauth_apps app ON app.id = credentials.oauth_app_id
                WHERE grants.principal_id = $1
                  AND credentials.dead = FALSE
                  AND credentials.provider_subject <> ''
            )
            SELECT NULLIF(BTRIM(p.slack_user_id), '') AS slack_user_id,
                   ARRAY(
                       SELECT DISTINCT provider_subject FROM granted
                       WHERE provider = 'google' AND slug = $2
                       ORDER BY provider_subject
                   ) AS google_subjects,
                   ARRAY(
                       SELECT DISTINCT provider_subject FROM granted
                       WHERE provider = 'granola' AND slug = $3
                       ORDER BY provider_subject
                   ) AS granola_subjects
            FROM principals p
            WHERE p.id = $1
            "#,
        )
        .bind(principal_id)
        .bind(&self.google_oauth_app_slug)
        .bind(&self.granola_oauth_app_slug)
        .fetch_optional(&self.pool)
        .await
        .context("load principal from Rails Console")
    }

    pub async fn ready(&self) -> bool {
        let Ok(ids) = self.google_credential_ids().await else {
            return false;
        };
        let Some(id) = ids.first() else {
            return false;
        };
        self.google_credential(*id).await.is_ok()
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

#[cfg(test)]
mod tests {
    use std::env;

    use sqlx::Executor;

    use super::*;
    use crate::test_support::TestDatabase;

    #[tokio::test]
    async fn principals_are_known_by_their_granted_credentials() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "principal_identity").await;
        let pool = &database.pool;
        // The Console tables the identity lookup reads. Ada (1) holds direct
        // grants for a Google credential and two live Granola credentials,
        // plus a dead one and one from another app. Bob (2) has Granola only
        // through a role grant, and a Google subject only as a label.
        pool.execute(
            r#"
            CREATE TABLE principals (
                id bigint PRIMARY KEY, labels jsonb NOT NULL DEFAULT '{}', slack_user_id text
            );
            CREATE TABLE oauth_apps (id bigint PRIMARY KEY, provider text NOT NULL, slug text NOT NULL);
            CREATE TABLE broker_credentials (
                id bigint PRIMARY KEY, oauth_app_id bigint, provider_subject text,
                provider_email text, dead boolean NOT NULL DEFAULT false
            );
            CREATE TABLE static_secrets (id bigint PRIMARY KEY, broker_credential_id bigint);
            CREATE TABLE grants (principal_id bigint, role_id bigint, static_secret_id bigint);

            INSERT INTO principals VALUES
                (1, '{}', 'U-ADA'),
                (2, '{"google_subject": "G-BOB"}', NULL);
            INSERT INTO oauth_apps VALUES
                (1, 'granola', 'granola'), (2, 'granola', 'other'), (3, 'google', 'google');
            INSERT INTO broker_credentials VALUES
                (1, 1, 'GR-ADA', 'ada@example.com', false),
                (2, 1, 'GR-ADA-2', 'ada@example.com', false),
                (3, 1, 'GR-DEAD', 'ada@example.com', true),
                (4, 2, 'GR-OTHER-APP', 'ada@example.com', false),
                (5, 3, 'G-ADA', 'ada@example.com', false),
                (6, 1, 'GR-BOB', 'bob@example.com', false);
            INSERT INTO static_secrets SELECT id, id FROM broker_credentials;
            INSERT INTO grants VALUES
                (1, NULL, 1), (1, NULL, 2), (1, NULL, 3), (1, NULL, 4), (1, NULL, 5),
                (NULL, 1, 6);
            "#,
        )
        .await
        .unwrap();
        let credentials = ConsoleCredentials {
            pool: pool.clone(),
            encryption: Arc::new(ActiveRecordEncryption::new("primary", "salt")),
            google_oauth_app_slug: "google".to_owned(),
            granola_oauth_app_slug: "granola".to_owned(),
            slack_oauth_app_slug: "slack".to_owned(),
            google_user_emails: Vec::new(),
            granola_user_emails: Vec::new(),
            slack_user_ids: Vec::new(),
            slack_conversation_types: Vec::new(),
        };

        let ada = credentials.principal_identity(1).await.unwrap().unwrap();
        assert_eq!(ada.slack_user_id.as_deref(), Some("U-ADA"));
        assert_eq!(ada.google_subjects, ["G-ADA"]);
        assert_eq!(ada.granola_subjects, ["GR-ADA", "GR-ADA-2"]);
        let bob = credentials.principal_identity(2).await.unwrap().unwrap();
        assert!(bob.google_subjects.is_empty());
        assert!(bob.granola_subjects.is_empty());
        assert!(credentials.principal_identity(3).await.unwrap().is_none());

        database.drop().await;
    }
}
