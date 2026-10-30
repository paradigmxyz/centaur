//! Tests against a Postgres database holding Console's schema.
//!
//! Set `CONSOLE_DB_TEST_DATABASE_URL` to a database prepared with Console's
//! `bin/rails db:prepare`. Each test inserts uniquely named rows, so tests can
//! share one database and run in parallel.

use std::env;

use active_record_encryption::ActiveRecordEncryption;
use centaur_console_db::{
    REDACTED, SecretValues, SlackChannelPermission, Subject, build_config,
    find_direct_slack_channel_permission, find_principal, find_principal_by_foreign_id,
    find_user_id_by_email, models::Config,
};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};

const PRIMARY_KEY: &str = "dev_ar_encryption_primary_key_0000000000000000";
const SALT: &str = "dev_ar_encryption_key_derivation_salt_000000000";
/// "known secret" encrypted by Active Record with the keys above.
const ENCRYPTED: &str =
    r#"{"p":"xOiyro9XYBkfABUF","h":{"iv":"pCjJC5SxH78ZN0Nm","at":"s9DIUyHAkF8ZAthKO6ROtw=="}}"#;
const PLAINTEXT: &str = "known secret";

async fn database() -> Option<PgPool> {
    let Ok(url) = env::var("CONSOLE_DB_TEST_DATABASE_URL") else {
        assert!(
            env::var_os("CI").is_none(),
            "CONSOLE_DB_TEST_DATABASE_URL must be set in CI"
        );
        eprintln!("skipping: set CONSOLE_DB_TEST_DATABASE_URL to a database with Console's schema");
        return None;
    };
    Some(
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect to the test database"),
    )
}

struct Fixture {
    pool: PgPool,
    admin: i64,
    unique: String,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let pool = database().await?;
        let unique = format!("{:032x}", rand::random::<u128>());
        let admin = sqlx::query_scalar(
            "INSERT INTO users (email, admin, status, created_at, updated_at) \
             VALUES ($1, true, 'active', now(), now()) RETURNING id",
        )
        .bind(format!("admin-{unique}@example.com"))
        .fetch_one(&pool)
        .await
        .unwrap();
        Some(Self {
            pool,
            admin,
            unique,
        })
    }

    fn name(&self, label: &str) -> String {
        format!("{label}-{}", self.unique)
    }

    async fn principal(&self, label: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO principals (foreign_id, name, kind, labels, slack_user_id, created_by_id, created_at, updated_at) \
             VALUES ($1, 'Example', 'slack_dm', '{\"team\": \"core\"}', 'U12345678', $2, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn grant(&self, principal: i64, column: &str, secret: i64) {
        sqlx::query(&format!(
            "INSERT INTO grants (principal_id, {column}, priority, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, 100, $3, now(), now())"
        ))
        .bind(principal)
        .bind(secret)
        .bind(self.admin)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn rule(&self, column: &str, secret: i64, host: &str) {
        sqlx::query(&format!(
            "INSERT INTO request_rules ({column}, host, position, created_at, updated_at) \
             VALUES ($1, $2, 0, now(), now())"
        ))
        .bind(secret)
        .bind(host)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn static_secret(&self, label: &str, inject: Value, replace: Option<Value>) -> i64 {
        let (inject, replace) = match replace {
            Some(replace) => (None, Some(replace)),
            None => (Some(inject), None),
        };
        sqlx::query_scalar(
            "INSERT INTO static_secrets (foreign_id, inject_config, replace_config, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(inject)
        .bind(replace)
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn broker_credential(&self, label: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO broker_credentials (foreign_id, token_endpoint, access_token, created_by_id, created_at, updated_at) \
             VALUES ($1, 'https://idp.example/token', $2, $3, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(ENCRYPTED)
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn source(
        &self,
        column: &str,
        owner: i64,
        source_type: &str,
        secret: Option<&str>,
        broker: Option<i64>,
    ) {
        sqlx::query(&format!(
            "INSERT INTO secret_sources ({column}, source_type, config, secret, broker_credential_id, created_at, updated_at) \
             VALUES ($1, $2, '{{}}', $3, $4, now(), now())"
        ))
        .bind(owner)
        .bind(source_type)
        .bind(secret)
        .bind(broker)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// Grants `principal` a stored inject secret, a brokered secret, a replace
    /// secret, and a GCP keyfile transform, all with encrypted values.
    async fn grant_secret_values(&self, principal: i64) {
        let inject = self
            .static_secret("inject", json!({ "header": "X-Api-Key" }), None)
            .await;
        self.source(
            "static_secret_id",
            inject,
            "control_plane",
            Some(ENCRYPTED),
            None,
        )
        .await;
        self.rule("static_secret_id", inject, "inject.example")
            .await;
        self.grant(principal, "static_secret_id", inject).await;

        let broker = self.broker_credential("broker").await;
        let brokered = self
            .static_secret("brokered", json!({ "header": "Authorization" }), None)
            .await;
        self.source(
            "static_secret_id",
            brokered,
            "token_broker",
            None,
            Some(broker),
        )
        .await;
        self.rule("static_secret_id", brokered, "brokered.example")
            .await;
        self.grant(principal, "static_secret_id", brokered).await;

        let replace = self
            .static_secret(
                "replace",
                Value::Null,
                Some(json!({ "proxy_value": "PLACEHOLDER", "match_headers": ["X-Token"] })),
            )
            .await;
        self.source(
            "static_secret_id",
            replace,
            "control_plane",
            Some(ENCRYPTED),
            None,
        )
        .await;
        self.rule("static_secret_id", replace, "replace.example")
            .await;
        self.grant(principal, "static_secret_id", replace).await;

        let gcp: i64 = sqlx::query_scalar(
            "INSERT INTO gcp_auth_secrets (foreign_id, scopes, created_by_id, created_at, updated_at) \
             VALUES ($1, '[]', $2, now(), now()) RETURNING id",
        )
        .bind(self.name("gcp"))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        self.source(
            "gcp_auth_secret_id",
            gcp,
            "control_plane",
            Some(ENCRYPTED),
            None,
        )
        .await;
        self.rule("gcp_auth_secret_id", gcp, "gcp.example").await;
        self.grant(principal, "gcp_auth_secret_id", gcp).await;
    }
}

fn to_json(config: &Config) -> Value {
    json!({
        "secrets": config.secrets,
        "transforms": config.transforms,
        "postgres": config.postgres,
    })
}

/// Console's redaction: every inline control_plane value becomes `[redacted]`.
fn redact(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut redacted: serde_json::Map<String, Value> = map
                .iter()
                .map(|(key, value)| (key.clone(), redact(value)))
                .collect();
            if map.get("type") == Some(&json!("control_plane")) && map.contains_key("value") {
                redacted.insert("value".to_owned(), json!(REDACTED));
            }
            Value::Object(redacted)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact).collect()),
        other => other.clone(),
    }
}

#[tokio::test]
async fn redacted_config_is_the_live_config_without_secret_values() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let principal = fixture.principal("principal").await;
    fixture.grant_secret_values(principal).await;
    let empty = json!([]);
    let labels = json!({});
    let subject = Subject {
        principal_id: principal,
        principal: None,
        console_user_id: None,
        console_user_email: None,
        slack_history_channel_ids: &empty,
        proxy_labels: &labels,
    };
    let encryption = ActiveRecordEncryption::new(PRIMARY_KEY, SALT);

    let live = build_config(
        &fixture.pool,
        SecretValues::Live(&encryption),
        &subject,
        None,
    )
    .await
    .unwrap();
    let redacted = build_config(&fixture.pool, SecretValues::Redacted, &subject, None)
        .await
        .unwrap();

    let live = to_json(&live);
    assert_eq!(live["secrets"].as_array().unwrap().len(), 3);
    assert!(live.to_string().contains(PLAINTEXT));
    let redacted = to_json(&redacted);
    assert!(!redacted.to_string().contains(PLAINTEXT));
    assert_eq!(redacted, redact(&live));
}

#[tokio::test]
async fn effective_config_is_redacted_and_absent_for_missing_principals() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let principal = fixture.principal("principal").await;
    fixture.grant_secret_values(principal).await;

    let config = centaur_console_db::effective_config(&fixture.pool, principal)
        .await
        .unwrap()
        .expect("principal exists");
    let config = to_json(&config);
    assert!(!config.to_string().contains(PLAINTEXT));
    assert_eq!(
        config["secrets"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|secret| secret["replace"]["proxy_value"].as_str())
            .collect::<Vec<_>>(),
        vec!["PLACEHOLDER"]
    );

    assert!(
        centaur_console_db::effective_config(&fixture.pool, i64::MAX)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn reads_principals_users_and_direct_slack_permissions() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let principal = fixture.principal("principal").await;
    sqlx::query(
        "INSERT INTO slack_channel_permissions (principal_id, channel_id, upload_enabled, download_enabled, history_enabled, created_at, updated_at) \
         VALUES ($1, 'D12345678', true, false, true, now(), now())",
    )
    .bind(principal)
    .execute(&fixture.pool)
    .await
    .unwrap();

    let by_id = find_principal(&fixture.pool, principal)
        .await
        .unwrap()
        .expect("principal exists");
    assert_eq!(
        by_id.foreign_id.as_deref(),
        Some(fixture.name("principal").as_str())
    );
    assert_eq!(by_id.name.as_deref(), Some("Example"));
    assert_eq!(by_id.kind, "slack_dm");
    assert_eq!(by_id.labels, json!({ "team": "core" }));
    assert_eq!(by_id.slack_user_id.as_deref(), Some("U12345678"));
    assert_eq!(by_id.sandbox_repo_cache, "all");
    assert!(by_id.sandbox_observability_enabled);
    assert_eq!(
        find_principal_by_foreign_id(&fixture.pool, &fixture.name("principal"))
            .await
            .unwrap(),
        Some(by_id)
    );
    assert_eq!(
        find_principal_by_foreign_id(&fixture.pool, &fixture.name("missing"))
            .await
            .unwrap(),
        None
    );

    assert_eq!(
        find_user_id_by_email(
            &fixture.pool,
            &format!("admin-{}@example.com", fixture.unique)
        )
        .await
        .unwrap(),
        Some(fixture.admin)
    );
    assert_eq!(
        find_direct_slack_channel_permission(&fixture.pool, principal, "D12345678")
            .await
            .unwrap(),
        Some(SlackChannelPermission {
            upload_enabled: true,
            download_enabled: false,
            history_enabled: true,
        })
    );
    assert_eq!(
        find_direct_slack_channel_permission(&fixture.pool, principal, "D99999999")
            .await
            .unwrap(),
        None
    );
}
