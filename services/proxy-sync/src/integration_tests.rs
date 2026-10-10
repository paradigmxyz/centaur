//! End-to-end tests against a Postgres database holding Console's schema.
//!
//! Set `PROXY_SYNC_TEST_DATABASE_URL` to a database prepared with Console's
//! `bin/rails db:schema:load`. Each test inserts its own uniquely named rows,
//! so tests can share one database and run in parallel.

use std::{env, sync::Arc};

use active_record_encryption::ActiveRecordEncryption;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use metrics_exporter_prometheus::PrometheusBuilder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;

use crate::{cache::SyncCache, models::AppState, router};

const PRIMARY_KEY: &str = "dev_ar_encryption_primary_key_0000000000000000";
const SALT: &str = "dev_ar_encryption_key_derivation_salt_000000000";
/// "known secret" encrypted by Active Record with the keys above.
const ENCRYPTED: &str =
    r#"{"p":"xOiyro9XYBkfABUF","h":{"iv":"pCjJC5SxH78ZN0Nm","at":"s9DIUyHAkF8ZAthKO6ROtw=="}}"#;
const JWT_SECRET: &str = "integration-test-signing-key";

// Console's grant priorities: direct grants default to 100, role grants to 0.
const DIRECT: i32 = 100;
const ROLE: i32 = 0;

async fn database() -> Option<PgPool> {
    let Ok(url) = env::var("PROXY_SYNC_TEST_DATABASE_URL") else {
        assert!(
            env::var_os("CI").is_none(),
            "PROXY_SYNC_TEST_DATABASE_URL must be set in CI"
        );
        eprintln!("skipping: set PROXY_SYNC_TEST_DATABASE_URL to a database with Console's schema");
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
    state: AppState,
    admin: i64,
    unique: String,
}

#[derive(Clone, Copy)]
enum Grantee {
    Principal(i64),
    Role(i64),
}

impl Fixture {
    async fn new() -> Option<Self> {
        Self::with_jwt(false).await
    }

    async fn with_jwt(enabled: bool) -> Option<Self> {
        let pool = database().await?;
        let unique = format!("{:032x}", rand_suffix());
        let state = AppState {
            pool: pool.clone(),
            encryption: Arc::new(ActiveRecordEncryption::new(PRIMARY_KEY, SALT)),
            jwt_secret: enabled.then(|| JWT_SECRET.to_owned()),
            api_hosts: if enabled {
                vec!["api.internal".to_owned()]
            } else {
                vec![]
            },
            console_host: None,
            sync_cache: Arc::new(SyncCache::default()),
            metrics: PrometheusBuilder::new().build_recorder().handle(),
        };
        let admin = sqlx::query_scalar(
            "INSERT INTO users (email, admin, status, created_at, updated_at) \
             VALUES ($1, true, 'approved', now(), now()) RETURNING id",
        )
        .bind(format!("admin-{unique}@example.com"))
        .fetch_one(&pool)
        .await
        .unwrap();
        Some(Self {
            pool,
            state,
            admin,
            unique,
        })
    }

    fn name(&self, label: &str) -> String {
        format!("{label}-{}", self.unique)
    }

    async fn principal(&self, label: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO principals (foreign_id, kind, created_by_id, created_at, updated_at) \
             VALUES ($1, 'user', $2, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn role(&self, label: &str, principal: i64) -> i64 {
        let role: i64 = sqlx::query_scalar(
            "INSERT INTO roles (foreign_id, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) \
             VALUES ($1, $2, now(), now())",
        )
        .bind(principal)
        .bind(role)
        .execute(&self.pool)
        .await
        .unwrap();
        role
    }

    async fn grant(&self, grantee: Grantee, column: &str, secret: i64, priority: i32) {
        let (principal, role) = match grantee {
            Grantee::Principal(id) => (Some(id), None),
            Grantee::Role(id) => (None, Some(id)),
        };
        sqlx::query(&format!(
            "INSERT INTO grants (principal_id, role_id, {column}, priority, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, now(), now())"
        ))
        .bind(principal)
        .bind(role)
        .bind(secret)
        .bind(priority)
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

    async fn source(
        &self,
        column: &str,
        owner: i64,
        source_type: &str,
        config: Value,
        broker: Option<i64>,
    ) {
        sqlx::query(&format!(
            "INSERT INTO secret_sources ({column}, source_type, config, broker_credential_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, now(), now())"
        ))
        .bind(owner)
        .bind(source_type)
        .bind(config)
        .bind(broker)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn static_secret_row(&self, label: &str, header: &str, broker: Option<i64>) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO static_secrets (foreign_id, inject_config, broker_credential_id, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(json!({ "header": header }))
        .bind(broker)
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// A static secret injecting `header` on `host` from an env source.
    async fn static_secret(&self, label: &str, header: &str, host: &str) -> i64 {
        let secret = self.static_secret_row(label, header, None).await;
        self.source(
            "static_secret_id",
            secret,
            "env",
            json!({ "var": label }),
            None,
        )
        .await;
        self.rule("static_secret_id", secret, host).await;
        secret
    }

    async fn oauth_app(&self, label: &str, always_available: bool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO oauth_apps (slug, provider, client_id, always_available, created_by_id, created_at, updated_at) \
             VALUES ($1, 'github', 'client', $2, $3, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(always_available)
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn broker_credential(&self, label: &str, app: Option<i64>, minted: bool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO broker_credentials (foreign_id, token_endpoint, oauth_app_id, access_token, created_by_id, created_at, updated_at) \
             VALUES ($1, 'https://idp.example/token', $2, $3, $4, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(app)
        .bind(minted.then_some(ENCRYPTED))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// A static wrapper serving `served_by`'s token, linked to `linked` (the
    /// credential whose OAuth app gates requester hoisting).
    async fn wrapper_secret(&self, label: &str, linked: i64, served_by: i64, host: &str) -> i64 {
        let secret = self
            .static_secret_row(label, "Authorization", Some(linked))
            .await;
        self.source(
            "static_secret_id",
            secret,
            "token_broker",
            json!({ "credential_id": served_by }),
            Some(served_by),
        )
        .await;
        self.rule("static_secret_id", secret, host).await;
        secret
    }

    async fn gcp_auth(&self, label: &str, host: &str) -> i64 {
        let secret = sqlx::query_scalar(
            "INSERT INTO gcp_auth_secrets (foreign_id, credentials_provider, scopes, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(json!({ "type": "workload_identity" }))
        .bind(json!(["https://www.googleapis.com/auth/cloud-platform"]))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        self.rule("gcp_auth_secret_id", secret, host).await;
        secret
    }

    async fn pg_dsn(&self, label: &str, database: &str, settings: Value) -> i64 {
        let secret = sqlx::query_scalar(
            "INSERT INTO pg_dsn_secrets (foreign_id, database, settings, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(database)
        .bind(settings)
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        self.source(
            "pg_dsn_secret_id",
            secret,
            "env",
            json!({ "var": label }),
            None,
        )
        .await;
        secret
    }

    async fn slack_permission(
        &self,
        column: &str,
        owner: i64,
        channel: &str,
        upload: bool,
        history: bool,
    ) {
        sqlx::query(&format!(
            "INSERT INTO slack_channel_permissions ({column}, channel_id, upload_enabled, history_enabled, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, now(), now())"
        ))
        .bind(owner)
        .bind(channel)
        .bind(upload)
        .bind(history)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// Registers a proxy and returns its bearer token.
    async fn proxy(&self, principal: Option<i64>, requester: Option<i64>, labels: Value) -> String {
        let token = format!("iprx_{:032x}{:032x}", rand_suffix(), rand_suffix());
        sqlx::query(
            "INSERT INTO proxies (name, bearer_token_hash, principal_id, requester_principal_id, labels, \
                                  principal_assigned_at, requester_principal_assigned_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, now(), now(), now(), now())",
        )
        .bind(self.name("proxy"))
        .bind(hex::encode(Sha256::digest(token.as_bytes())))
        .bind(principal)
        .bind(requester)
        .bind(labels)
        .execute(&self.pool)
        .await
        .unwrap();
        token
    }

    async fn update(&self, sql: &str, id: i64) {
        sqlx::query(sql).bind(id).execute(&self.pool).await.unwrap();
    }

    /// Console bumps a principal's cache version whenever its config changes.
    async fn bump(&self, principal: i64) {
        self.update(
            "UPDATE principals SET sync_config_cache_version = sync_config_cache_version + 1 \
             WHERE id = $1",
            principal,
        )
        .await;
    }

    async fn post(&self, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
        let mut request = Request::post(path).header("content-type", "application/json");
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = router(self.state.clone())
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn sync(&self, token: &str) -> Value {
        let (status, body) = self
            .post("/api/v1/proxy/sync", Some(token), json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }
}

fn rand_suffix() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    nanos
        ^ (u128::from(std::process::id()) << 64)
        ^ u128::from(COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// Hosts of the served static secrets, in served order.
fn secret_hosts(body: &Value) -> Vec<String> {
    body["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|secret| secret["rules"][0]["host"].as_str().unwrap_or("").to_owned())
        .collect()
}

fn transform_names(body: &Value) -> Vec<String> {
    body["transforms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|transform| transform["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn role_and_direct_grants_resolve_to_the_strongest_priority() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let principal = f.principal("principal").await;
    let role = f.role("role", principal).await;
    let shared = f
        .static_secret("shared", "Authorization", "api.example.com")
        .await;
    f.grant(Grantee::Role(role), "static_secret_id", shared, ROLE)
        .await;
    f.grant(
        Grantee::Principal(principal),
        "static_secret_id",
        shared,
        DIRECT,
    )
    .await;
    let gcp = f.gcp_auth("gcp", "api.example.com").await;
    f.grant(Grantee::Role(role), "gcp_auth_secret_id", gcp, 50)
        .await;
    let token = f.proxy(Some(principal), None, json!({})).await;

    // The secret granted both ways is served once at its direct priority, so
    // it beats the priority-50 role transform writing the same header.
    let body = f.sync(&token).await;
    assert_eq!(body["status"], "assigned");
    assert_eq!(secret_hosts(&body), vec!["api.example.com"]);
    assert!(transform_names(&body).is_empty());

    // Promoting the role transform above the direct grant flips the winner.
    f.update(
        "UPDATE grants SET priority = 900 WHERE gcp_auth_secret_id = $1",
        gcp,
    )
    .await;
    f.bump(principal).await;
    let body = f.sync(&token).await;
    assert!(secret_hosts(&body).is_empty());
    assert_eq!(transform_names(&body), vec!["gcp_auth"]);
}

#[tokio::test]
async fn undeliverable_brokered_secrets_are_dropped_before_deconfliction() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let principal = f.principal("principal").await;
    let role = f.role("role", principal).await;
    let unminted = f.broker_credential("unminted", None, false).await;
    let wrapper = f
        .wrapper_secret("unminted", unminted, unminted, "api.example.com")
        .await;
    f.grant(
        Grantee::Principal(principal),
        "static_secret_id",
        wrapper,
        DIRECT,
    )
    .await;
    let gcp = f.gcp_auth("gcp", "api.example.com").await;
    f.grant(Grantee::Role(role), "gcp_auth_secret_id", gcp, ROLE)
        .await;
    let token = f.proxy(Some(principal), None, json!({})).await;

    let body = f.sync(&token).await;
    assert!(secret_hosts(&body).is_empty());
    assert_eq!(transform_names(&body), vec!["gcp_auth"]);

    // Once minted, the token is delivered inline and wins the header.
    f.update(
        &format!("UPDATE broker_credentials SET access_token = '{ENCRYPTED}' WHERE id = $1"),
        unminted,
    )
    .await;
    f.bump(principal).await;
    let body = f.sync(&token).await;
    assert_eq!(
        body["secrets"][0]["source"],
        json!({ "type": "control_plane", "value": "known secret" })
    );
    assert!(transform_names(&body).is_empty());
}

#[tokio::test]
async fn requesters_contribute_only_direct_always_available_wrappers() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let principal = f.principal("conversation").await;
    let requester = f.principal("requester").await;
    let requester_role = f.role("requester-role", requester).await;

    let always = f.oauth_app("always", true).await;
    let gated = f.oauth_app("gated", false).await;
    let hoisted = f.broker_credential("hoisted", Some(always), true).await;
    let role_granted = f
        .broker_credential("role-granted", Some(always), true)
        .await;
    let linked = f.broker_credential("linked", Some(always), true).await;
    let other = f.broker_credential("other", None, true).await;
    let not_whitelisted = f
        .broker_credential("not-whitelisted", Some(gated), true)
        .await;
    let unminted = f.broker_credential("unminted", Some(always), false).await;

    let cases = [
        (
            f.wrapper_secret("hoisted", hoisted, hoisted, "hoisted.example.com")
                .await,
            Grantee::Principal(requester),
        ),
        (
            f.wrapper_secret(
                "via-role",
                role_granted,
                role_granted,
                "via-role.example.com",
            )
            .await,
            Grantee::Role(requester_role),
        ),
        (
            f.wrapper_secret(
                "gated",
                not_whitelisted,
                not_whitelisted,
                "gated.example.com",
            )
            .await,
            Grantee::Principal(requester),
        ),
        (
            f.wrapper_secret("mismatched", linked, other, "mismatched.example.com")
                .await,
            Grantee::Principal(requester),
        ),
        (
            f.wrapper_secret("unminted", unminted, unminted, "unminted.example.com")
                .await,
            Grantee::Principal(requester),
        ),
        (
            f.static_secret("plain", "X-Plain", "plain.example.com")
                .await,
            Grantee::Principal(requester),
        ),
    ];
    for (secret, grantee) in cases {
        f.grant(grantee, "static_secret_id", secret, DIRECT).await;
    }
    let gcp = f
        .gcp_auth("requester-gcp", "requester-gcp.example.com")
        .await;
    f.grant(
        Grantee::Principal(requester),
        "gcp_auth_secret_id",
        gcp,
        DIRECT,
    )
    .await;
    let own = f
        .static_secret("conversation", "X-Conversation", "conversation.example.com")
        .await;
    f.grant(Grantee::Principal(principal), "static_secret_id", own, ROLE)
        .await;

    let without = f.proxy(Some(principal), None, json!({})).await;
    assert_eq!(
        secret_hosts(&f.sync(&without).await),
        vec!["conversation.example.com"]
    );

    let token = f.proxy(Some(principal), Some(requester), json!({})).await;
    let body = f.sync(&token).await;
    assert_eq!(
        secret_hosts(&body),
        vec!["conversation.example.com", "hoisted.example.com"]
    );
    assert!(transform_names(&body).is_empty());

    // A requester on an unassigned proxy contributes nothing.
    let unassigned = f.proxy(None, Some(requester), json!({})).await;
    let body = f.sync(&unassigned).await;
    assert_eq!(body["status"], "unassigned");
    assert!(secret_hosts(&body).is_empty());
}

#[tokio::test]
async fn postgres_routes_pick_the_strongest_grant_and_render_settings() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let principal = f.principal("principal").await;
    let role = f.role("role", principal).await;
    let database = f.name("analytics");
    let weak = f.pg_dsn("weak", &database, json!([])).await;
    let strong = f
        .pg_dsn(
            "strong",
            &database,
            json!([
                { "name": "app.principal", "value_from": { "principal_field": "foreign_id" } },
                { "name": "app.sandbox", "value_from": { "proxy_label": "sandbox" } },
                { "name": "app.missing", "value_from": { "proxy_label": "missing" } },
                { "name": "app.fixed", "value": "constant" }
            ]),
        )
        .await;
    f.grant(
        Grantee::Principal(principal),
        "pg_dsn_secret_id",
        weak,
        DIRECT,
    )
    .await;
    f.grant(Grantee::Role(role), "pg_dsn_secret_id", strong, 900)
        .await;
    let token = f
        .proxy(Some(principal), None, json!({ "sandbox": "sb-1" }))
        .await;

    let body = f.sync(&token).await;
    let postgres = body["postgres"].as_array().unwrap();
    assert_eq!(postgres.len(), 1);
    assert_eq!(postgres[0]["foreign_id"], f.name("strong"));
    assert_eq!(
        postgres[0]["dsn"],
        json!({ "type": "env", "var": "strong" })
    );
    assert_eq!(
        postgres[0]["settings"],
        json!([
            { "name": "app.principal", "value": f.name("principal") },
            { "name": "app.sandbox", "value": "sb-1" },
            { "name": "app.missing", "value": "" },
            { "name": "app.fixed", "value": "constant" }
        ])
    );
}

#[tokio::test]
async fn api_jwt_carries_direct_and_role_slack_permissions() {
    let Some(f) = Fixture::with_jwt(true).await else {
        return;
    };
    let principal = f.principal("principal").await;
    let role = f.role("role", principal).await;
    f.slack_permission("principal_id", principal, "C0000000001", true, false)
        .await;
    f.slack_permission("role_id", role, "C0000000001", false, true)
        .await;
    f.slack_permission("role_id", role, "C0000000002", false, false)
        .await;
    let token = f.proxy(Some(principal), None, json!({})).await;

    let body = f.sync(&token).await;
    let secret = &body["secrets"][0];
    assert_eq!(secret["rules"], json!([{ "host": "api.internal" }]));
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.set_audience(&["centaur-api"]);
    validation.set_issuer(&["centaur-console"]);
    let claims = jsonwebtoken::decode::<Value>(
        secret["source"]["value"].as_str().unwrap(),
        &jsonwebtoken::DecodingKey::from_secret(JWT_SECRET.as_bytes()),
        &validation,
    )
    .unwrap()
    .claims;
    assert_eq!(
        claims["slack"],
        json!({
            "upload_channels": ["C0000000001"],
            "download_channels": [],
            "history_channels": ["C0000000001"]
        })
    );
}

#[tokio::test]
async fn sync_returns_only_the_hash_until_grants_change() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let principal = f.principal("principal").await;
    let first = f
        .static_secret("first", "X-First", "first.example.com")
        .await;
    f.grant(
        Grantee::Principal(principal),
        "static_secret_id",
        first,
        DIRECT,
    )
    .await;
    let token = f.proxy(Some(principal), None, json!({})).await;

    let hash = f.sync(&token).await["config_hash"].clone();
    let (_, unchanged) = f
        .post(
            "/api/v1/proxy/sync",
            Some(&token),
            json!({ "config_hash": hash }),
        )
        .await;
    assert_eq!(unchanged, json!({ "config_hash": hash }));

    // Console bumps the principal's cache version on every grant change.
    let second = f
        .static_secret("second", "X-Second", "second.example.com")
        .await;
    f.grant(
        Grantee::Principal(principal),
        "static_secret_id",
        second,
        DIRECT,
    )
    .await;
    f.bump(principal).await;
    let (_, changed) = f
        .post(
            "/api/v1/proxy/sync",
            Some(&token),
            json!({ "config_hash": hash }),
        )
        .await;
    assert_ne!(changed["config_hash"], hash);
    assert_eq!(
        secret_hosts(&changed),
        vec!["first.example.com", "second.example.com"]
    );
}

#[tokio::test]
async fn unknown_or_missing_proxy_tokens_are_rejected() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    for token in [None, Some("iprx_unknown")] {
        let (status, body) = f.post("/api/v1/proxy/sync", token, json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["message"], "invalid or missing proxy token");
    }
}
