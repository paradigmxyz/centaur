//! End-to-end tests against a Postgres database holding Console's schema.
//!
//! Set `PROXY_SYNC_TEST_DATABASE_URL` to a database migrated with
//! `cargo run -p centaur-console-db --example migrate -- <url>`. Each test inserts its own uniquely named rows,
//! so tests can share one database and run in parallel.

use std::{env, sync::Arc};

use active_record_encryption::ActiveRecordEncryption;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use centaur_codemode_protocol::token::{
    TTL_SECS, ToolPolicy, TurnGrant, TurnTokenSigner, TurnTokenVerifier, testing,
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
const CODEMODE_SYNC_TOKEN: &str = "integration-test-codemode-sync-token";

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
    turn_signer: TurnTokenSigner,
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
        let (private, public) = testing::pair();
        let turn_signer = TurnTokenSigner::from_pem(private.as_bytes()).unwrap();
        let state = AppState {
            ssh_targets: vec![],
            pool: pool.clone(),
            encryption: Arc::new(ActiveRecordEncryption::new(PRIMARY_KEY, SALT)),
            jwt_secret: enabled.then(|| JWT_SECRET.to_owned()),
            codemode_sync_token: Some(CODEMODE_SYNC_TOKEN.to_owned()),
            codemode_token_verifier: enabled
                .then(|| TurnTokenVerifier::from_pem(public.as_bytes()).unwrap()),
            api_hosts: if enabled {
                vec!["api.internal".to_owned()]
            } else {
                vec![]
            },
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
            turn_signer,
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

    async fn static_secret_row(
        &self,
        label: &str,
        inject: Option<Value>,
        replace: Option<Value>,
        broker: Option<i64>,
    ) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO static_secrets (foreign_id, inject_config, replace_config, broker_credential_id, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(inject)
        .bind(replace)
        .bind(broker)
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// A static secret injecting `header` on `host` from an env source.
    async fn static_secret(&self, label: &str, header: &str, host: &str) -> i64 {
        let secret = self
            .static_secret_row(label, Some(json!({ "header": header })), None, None)
            .await;
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
            .static_secret_row(
                label,
                Some(json!({ "header": "Authorization" })),
                None,
                Some(linked),
            )
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

    /// A replace secret swapping `proxy_value` in `match_header` on `host`.
    async fn replace_secret(
        &self,
        label: &str,
        proxy_value: &str,
        match_header: &str,
        host: &str,
    ) -> i64 {
        let replace = json!({ "proxy_value": proxy_value, "match_headers": [match_header] });
        let secret = self
            .static_secret_row(label, None, Some(replace), None)
            .await;
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

    async fn oauth_token(&self, label: &str, header: Option<&str>, host: &str) -> i64 {
        let secret = sqlx::query_scalar(
            "INSERT INTO oauth_token_secrets (foreign_id, \"grant\", token_endpoint, header, scopes, created_by_id, created_at, updated_at) \
             VALUES ($1, 'refresh_token', 'https://oauth2.example/token', $2, $3, $4, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(header)
        .bind(json!(["read"]))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        // Console requires these credential fields for the refresh_token grant.
        for (field, var) in [
            ("refresh_token", "CONFLICT_REFRESH"),
            ("client_id", "CONFLICT_CLIENT"),
        ] {
            sqlx::query(
                "INSERT INTO secret_sources (oauth_token_secret_id, source_type, config, role, role_kind, created_at, updated_at) \
                 VALUES ($1, 'env', $2, $3, 'credential_field', now(), now())",
            )
            .bind(secret)
            .bind(json!({ "var": var }))
            .bind(field)
            .execute(&self.pool)
            .await
            .unwrap();
        }
        self.rule("oauth_token_secret_id", secret, host).await;
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

    /// Posts to the tenancy config route with the proxy credential and the
    /// tenant token in their respective headers.
    async fn codemode(
        &self,
        proxy_token: Option<&str>,
        tenant_token: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let mut request =
            Request::post(CODEMODE_ENDPOINT).header("content-type", "application/json");
        if let Some(token) = proxy_token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        if let Some(token) = tenant_token {
            request = request.header("x-iron-tenant-token", token);
        }
        let response = router(self.state.clone())
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    fn turn_token(&self, principal: i64, requester: Option<i64>, now: i64) -> String {
        turn_token(&self.turn_signer, principal, requester, now)
    }
}

/// Returns a suffix unique across tests sharing one database.
///
/// Each component owns disjoint bits, so values never collide the way XORing
/// them would: the counter makes values unique within this process, and the
/// process id and start time distinguish concurrent and repeated runs.
fn rand_suffix() -> u128 {
    use std::sync::{
        LazyLock,
        atomic::{AtomicU32, Ordering},
    };
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    static STARTED_NANOS: LazyLock<u64> = LazyLock::new(|| {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        u64::try_from(nanos & u128::from(u64::MAX)).unwrap()
    });
    (u128::from(std::process::id()) << 96)
        | (u128::from(*STARTED_NANOS) << 32)
        | u128::from(COUNTER.fetch_add(1, Ordering::Relaxed))
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

/// The credentials a sync serves on `*.conflict.test` hosts, as sorted
/// "kind host header" lines, so conflict outcomes compare without depending on
/// payload order.
fn served_conflict_credentials(body: &Value) -> Vec<String> {
    let host = |rules: &Value| rules[0]["host"].as_str().unwrap_or("").to_owned();
    let mut served: Vec<String> = body["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|secret| {
            let header = secret["inject"]["header"]
                .as_str()
                .or_else(|| secret["replace"]["match_headers"][0].as_str())
                .unwrap_or("");
            format!("static {} {header}", host(&secret["rules"]))
        })
        .collect();
    for transform in body["transforms"].as_array().unwrap() {
        let name = transform["name"].as_str().unwrap();
        if name == "oauth_token" {
            for token in transform["config"]["tokens"].as_array().unwrap() {
                let header = token["header"].as_str().unwrap_or("Authorization");
                served.push(format!("oauth_token {} {header}", host(&token["rules"])));
            }
        } else {
            served.push(format!("{name} {}", host(&transform["config"]["rules"])));
        }
    }
    served.retain(|line| {
        line.split(' ')
            .nth(1)
            .is_some_and(|host| host.ends_with(".conflict.test"))
    });
    served.sort();
    served
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
async fn conflicting_credentials_resolve_like_console() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let principal = f.principal("principal").await;
    let role = f.role("role", principal).await;
    let direct = Grantee::Principal(principal);
    let via_role = Grantee::Role(role);

    // A direct static secret beats a role transform on the same host and
    // header. An OAuth token on a custom header does not conflict with it,
    // but beats a weaker static secret writing that custom header.
    let api = "api.conflict.test";
    let s = f.static_secret("direct-auth", "Authorization", api).await;
    f.grant(direct, "static_secret_id", s, DIRECT).await;
    let s = f.gcp_auth("role-gcp", api).await;
    f.grant(via_role, "gcp_auth_secret_id", s, ROLE).await;
    let s = f
        .oauth_token("custom-oauth", Some("X-Api-Token"), api)
        .await;
    f.grant(direct, "oauth_token_secret_id", s, DIRECT).await;
    let s = f.static_secret("role-custom", "X-Api-Token", api).await;
    f.grant(via_role, "static_secret_id", s, ROLE).await;

    // A wildcard host conflicts with a matching exact host.
    let s = f
        .static_secret("wildcard", "Authorization", "*.wild.conflict.test")
        .await;
    f.grant(direct, "static_secret_id", s, DIRECT).await;
    let s = f.gcp_auth("wild-gcp", "bq.wild.conflict.test").await;
    f.grant(via_role, "gcp_auth_secret_id", s, ROLE).await;

    // A promoted role grant beats a direct grant.
    let promoted = "promoted.conflict.test";
    let s = f.static_secret("demoted", "Authorization", promoted).await;
    f.grant(direct, "static_secret_id", s, DIRECT).await;
    let s = f.gcp_auth("promoted-gcp", promoted).await;
    f.grant(via_role, "gcp_auth_secret_id", s, 900).await;

    // A replace secret claims its match headers.
    let slack = "slack.conflict.test";
    let s = f
        .replace_secret("bot-token", "SLACK_BOT_TOKEN", "Authorization", slack)
        .await;
    f.grant(via_role, "static_secret_id", s, ROLE).await;
    let s = f.static_secret("user-token", "Authorization", slack).await;
    f.grant(direct, "static_secret_id", s, DIRECT).await;

    // Equal priorities are left to the proxy.
    let equal = "equal.conflict.test";
    let s = f
        .static_secret("equal-static", "Authorization", equal)
        .await;
    f.grant(direct, "static_secret_id", s, DIRECT).await;
    let s = f.gcp_auth("equal-gcp", equal).await;
    f.grant(direct, "gcp_auth_secret_id", s, DIRECT).await;

    let token = f.proxy(Some(principal), None, json!({})).await;
    assert_eq!(
        served_conflict_credentials(&f.sync(&token).await),
        vec![
            "gcp_auth equal.conflict.test",
            "gcp_auth promoted.conflict.test",
            "oauth_token api.conflict.test X-Api-Token",
            "static *.wild.conflict.test Authorization",
            "static api.conflict.test Authorization",
            "static equal.conflict.test Authorization",
            "static slack.conflict.test Authorization",
        ]
    );
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
async fn sandbox_token_carries_direct_and_role_slack_permissions() {
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
    let secrets = body["secrets"].as_array().unwrap();
    assert_eq!(secrets.len(), 1, "one sandbox token per proxy");
    let secret = &secrets[0];
    assert_eq!(secret["rules"], json!([{ "host": "api.internal" }]));
    let claims = decode_sandbox_token(secret);
    assert_eq!(
        claims["slack"],
        json!({
            "upload_channels": ["C0000000001"],
            "download_channels": [],
            "history_channels": ["C0000000001"]
        })
    );
    assert_eq!(claims["sub"], centaur_console_db::oid("prn", principal));
}

#[tokio::test]
async fn one_sandbox_token_is_injected_for_api_and_console_hosts() {
    let Some(mut f) = Fixture::with_jwt(true).await else {
        return;
    };
    // Both surfaces share one api-rs listener, so both hosts receive the same
    // token and there is no second injection to order.
    f.state.api_hosts.push("console.internal".to_owned());
    let principal = f.principal("principal").await;
    let token = f.proxy(Some(principal), None, json!({})).await;

    let body = f.sync(&token).await;
    let secrets = body["secrets"].as_array().unwrap();
    assert_eq!(secrets.len(), 1);
    assert_eq!(
        secrets[0]["rules"],
        json!([{ "host": "api.internal" }, { "host": "console.internal" }])
    );
    let claims = decode_sandbox_token(&secrets[0]);
    assert_eq!(claims["sandbox_id"], f.name("proxy"));
    assert_eq!(
        claims["capabilities"],
        json!([
            "slack.proxy",
            "sandbox.permissions",
            "sandbox.skills",
            "sandbox.tasks",
            "sandbox.oauth_apps"
        ])
    );
    assert_eq!(claims["console_delegate"], false);
}

fn decode_sandbox_token(secret: &Value) -> Value {
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.set_audience(&[crate::tokens::SANDBOX_TOKEN_AUDIENCE]);
    validation.set_issuer(&[crate::tokens::SANDBOX_TOKEN_ISSUER]);
    jsonwebtoken::decode::<Value>(
        secret["source"]["value"].as_str().unwrap(),
        &jsonwebtoken::DecodingKey::from_secret(JWT_SECRET.as_bytes()),
        &validation,
    )
    .unwrap()
    .claims
}

#[tokio::test]
async fn console_delegate_is_computed_for_a_linked_admin_slack_dm() {
    let Some(f) = Fixture::with_jwt(true).await else {
        return;
    };
    let user: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, admin, status, created_at, updated_at) \
         VALUES ($1, true, 'active', now(), now()) RETURNING id",
    )
    .bind(format!("delegate-{}@example.com", f.unique))
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let principal: i64 = sqlx::query_scalar(
        "INSERT INTO principals \
            (foreign_id, kind, slack_team_id, console_user_id, created_by_id, created_at, updated_at) \
         VALUES ($1, 'slack_dm', 'T-delegate', $2, $3, now(), now()) RETURNING id",
    )
    .bind(f.name("delegate"))
    .bind(user)
    .bind(f.admin)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO slack_bot_channels \
            (team_id, channel_id, name, bot_user_id, created_at, updated_at) \
         VALUES ('T-delegate', $1, 'general', 'U-bot', now(), now())",
    )
    .bind(f.name("channel"))
    .execute(&f.pool)
    .await
    .unwrap();
    let token = f.proxy(Some(principal), None, json!({})).await;

    let body = f.sync(&token).await;
    let claims = decode_sandbox_token(&body["secrets"][0]);
    assert_eq!(claims["console_delegate"], true);
    assert!(
        claims["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("console.delegate"))
    );

    // The tenant variant carries the same capability set and the same
    // delegate rule, only bound to `codemode` instead of a proxy.
    let (status, codemode) = f
        .codemode(
            Some(CODEMODE_SYNC_TOKEN),
            Some(&f.turn_token(principal, None, now())),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{codemode}");
    let claims = decode_sandbox_token(codemode_sandbox_secret(&codemode));
    assert_eq!(claims["console_delegate"], true);
    assert!(
        claims["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("console.delegate"))
    );
}

const CODEMODE_ENDPOINT: &str = "/api/v1/proxy/tenant_config";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn turn_token(
    signer: &TurnTokenSigner,
    principal: i64,
    requester: Option<i64>,
    now: i64,
) -> String {
    let (token, _) = signer
        .sign(
            &TurnGrant {
                principal: centaur_console_db::oid("prn", principal),
                requester: requester.map(|id| centaur_console_db::oid("prn", id)),
                thread_key: "slack:T1:C1:1.2".to_owned(),
                execution_id: "exe_integration".to_owned(),
                tools: ToolPolicy::default(),
            },
            now,
        )
        .unwrap();
    token
}

/// The api-rs token secret in a principal config: the one injected on api-rs
/// hosts, always last because it is appended after the stored credentials.
fn codemode_sandbox_secret(body: &Value) -> &Value {
    body["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|secret| secret["rules"][0]["host"] == "api.internal")
        .expect("a codemode sandbox token")
}

#[tokio::test]
async fn tenant_config_serves_a_principal_without_a_proxy() {
    let Some(f) = Fixture::with_jwt(true).await else {
        return;
    };
    let principal = f.principal("codemode").await;
    let own = f
        .static_secret("codemode-own", "X-Own", "own.example.com")
        .await;
    f.grant(
        Grantee::Principal(principal),
        "static_secret_id",
        own,
        DIRECT,
    )
    .await;

    let requester = f.principal("codemode-requester").await;
    let app = f.oauth_app("codemode-always", true).await;
    let credential = f
        .broker_credential("codemode-hoisted", Some(app), true)
        .await;
    let hoisted = f
        .wrapper_secret(
            "codemode-hoisted",
            credential,
            credential,
            "hoisted.example.com",
        )
        .await;
    f.grant(
        Grantee::Principal(requester),
        "static_secret_id",
        hoisted,
        DIRECT,
    )
    .await;

    let token = f.turn_token(principal, None, now());
    let (status, body) = f
        .codemode(Some(CODEMODE_SYNC_TOKEN), Some(&token), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut hosts = secret_hosts(&body);
    hosts.sort();
    assert_eq!(hosts, vec!["api.internal", "own.example.com"]);
    assert!(body["config_hash"].as_str().unwrap().starts_with("sha256:"));
    // The tenancy contract carries no postgres block, and this config has no
    // top-level rules.
    assert!(body.get("postgres").is_none());
    assert!(body.get("rules").is_none());

    let claims = decode_sandbox_token(codemode_sandbox_secret(&body));
    assert_eq!(claims["binding"], "codemode");
    assert_eq!(claims["proxy_id"], "");
    assert_eq!(claims["sandbox_id"], "codemode");
    assert_eq!(claims["sub"], centaur_console_db::oid("prn", principal));
    assert_eq!(
        claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
        900
    );

    // The requester comes from the token and contributes only
    // always-available OAuth wrappers, exactly as on a proxy sync.
    let requester_token = f.turn_token(principal, Some(requester), now());
    let (status, with_requester) = f
        .codemode(Some(CODEMODE_SYNC_TOKEN), Some(&requester_token), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{with_requester}");
    let mut hosts = secret_hosts(&with_requester);
    hosts.sort();
    assert_eq!(
        hosts,
        vec!["api.internal", "hoisted.example.com", "own.example.com"]
    );
    assert_ne!(with_requester["config_hash"], body["config_hash"]);
}

#[tokio::test]
async fn tenant_config_requires_the_proxy_and_tenant_credentials() {
    let Some(mut f) = Fixture::with_jwt(true).await else {
        return;
    };
    let principal = f.principal("codemode-auth").await;
    let tenant = f.turn_token(principal, None, now());

    // The proxy credential is required and checked in constant time.
    for token in [None, Some("wrong-sync-token")] {
        let (status, body) = f.codemode(token, Some(&tenant), json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
        assert_eq!(body["error"]["message"], "invalid or missing sync token");
    }

    // A missing, expired, foreign-key, or malformed tenant token is refused.
    let (private, _) = testing::pair();
    let other_signer = TurnTokenSigner::from_pem(private.as_bytes()).unwrap();
    let other_key = turn_token(&other_signer, principal, None, now());
    let expired = f.turn_token(principal, None, now() - TTL_SECS - 120);
    for tenant_token in [
        None,
        Some(other_key.as_str()),
        Some(expired.as_str()),
        Some("not-a-token"),
    ] {
        let (status, body) = f
            .codemode(Some(CODEMODE_SYNC_TOKEN), tenant_token, json!({}))
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"]["message"], "invalid or missing tenant token");
    }

    // A valid token may name a principal that does not exist.
    for sub in [
        centaur_console_db::oid("prn", principal + 1_000_000),
        "not-an-oid".to_owned(),
    ] {
        let (unknown, _) = f
            .turn_signer
            .sign(
                &TurnGrant {
                    principal: sub,
                    requester: None,
                    thread_key: "slack:T1:C1:1.2".to_owned(),
                    execution_id: "exe_integration".to_owned(),
                    tools: ToolPolicy::default(),
                },
                now(),
            )
            .unwrap();
        let (status, body) = f
            .codemode(Some(CODEMODE_SYNC_TOKEN), Some(&unknown), json!({}))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["message"], "unknown principal");
    }

    // Without the environment value the route does not exist.
    f.state.codemode_sync_token = None;
    let (status, response) = f
        .codemode(Some(CODEMODE_SYNC_TOKEN), Some(&tenant), json!({}))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{response}");
    assert_eq!(response["error"]["message"], "route is not configured");
}

#[tokio::test]
async fn tenant_config_answers_hash_only_until_grants_change() {
    let Some(f) = Fixture::with_jwt(true).await else {
        return;
    };
    let principal = f.principal("codemode-hash").await;
    let first = f
        .static_secret("codemode-first", "X-First", "first.example.com")
        .await;
    f.grant(
        Grantee::Principal(principal),
        "static_secret_id",
        first,
        DIRECT,
    )
    .await;
    let token = f.turn_token(principal, None, now());

    let (status, full) = f
        .codemode(Some(CODEMODE_SYNC_TOKEN), Some(&token), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{full}");
    let hash = full["config_hash"].clone();
    let (status, unchanged) = f
        .codemode(
            Some(CODEMODE_SYNC_TOKEN),
            Some(&token),
            json!({ "config_hash": hash }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{unchanged}");
    assert_eq!(unchanged, json!({ "config_hash": hash }));

    // Console bumps the principal's cache version on every grant change.
    let second = f
        .static_secret("codemode-second", "X-Second", "second.example.com")
        .await;
    f.grant(
        Grantee::Principal(principal),
        "static_secret_id",
        second,
        DIRECT,
    )
    .await;
    f.bump(principal).await;
    let (status, changed) = f
        .codemode(
            Some(CODEMODE_SYNC_TOKEN),
            Some(&token),
            json!({ "config_hash": hash }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_ne!(changed["config_hash"], hash);
    assert_eq!(
        secret_hosts(&changed),
        vec!["first.example.com", "second.example.com", "api.internal"]
    );
}

#[tokio::test]
async fn codemode_api_token_is_stable_within_one_window() {
    let Some(f) = Fixture::with_jwt(true).await else {
        return;
    };
    let principal = f.principal("codemode-window").await;
    let token = f.turn_token(principal, None, now());
    // Two calls can straddle a window boundary; the next pair in the new window
    // proves the property.
    for _ in 0..5 {
        let (status, first) = f
            .codemode(Some(CODEMODE_SYNC_TOKEN), Some(&token), json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{first}");
        let (status, second) = f
            .codemode(Some(CODEMODE_SYNC_TOKEN), Some(&token), json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{second}");
        if first["config_hash"] != second["config_hash"] {
            continue;
        }
        assert_eq!(first["secrets"], second["secrets"]);
        let claims = decode_sandbox_token(codemode_sandbox_secret(&second));
        assert_eq!(
            claims["exp"].as_i64().unwrap() - claims["iat"].as_i64().unwrap(),
            900
        );
        return;
    }
    panic!("the config hash changed across every pair of calls in one window");
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

const SSH_ENDPOINT: &str = "/api/v1/proxy/ssh/authorize";

fn ssh_request(destination: &str) -> Value {
    json!({
        "route": "ee",
        "public_key": "ssh-ed25519 sandbox-key",
        "destination": destination,
    })
}

impl Fixture {
    fn allow_ssh_targets(&mut self) {
        self.state.ssh_targets = crate::ssh::target_policies(Some(
            r#"[{"host":"*.dev.example.com","path":"/events/events","username":"dev"}]"#,
        ))
        .unwrap();
    }

    /// An SSH key secret whose private key is the inline "known secret".
    async fn ssh_key(&self, label: &str, owner: Option<i64>) -> i64 {
        let key: i64 = sqlx::query_scalar(
            "INSERT INTO ssh_key_secrets (foreign_id, name, public_key, owner_id, created_by_id, created_at, updated_at) \
             VALUES ($1, $1, 'ssh-ed25519 AAAAfixture', $2, $3, now(), now()) RETURNING id",
        )
        .bind(self.name(label))
        .bind(owner)
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO secret_sources (ssh_key_secret_id, source_type, secret, created_at, updated_at) \
             VALUES ($1, 'control_plane', $2, now(), now())",
        )
        .bind(key)
        .bind(ENCRYPTED)
        .execute(&self.pool)
        .await
        .unwrap();
        key
    }

    async fn ssh(&self, token: &str) -> (StatusCode, Value) {
        self.post(
            SSH_ENDPOINT,
            Some(token),
            ssh_request("wss://one.dev.example.com/events/events"),
        )
        .await
    }

    async fn ssh_status(&self, token: &str) -> StatusCode {
        self.ssh(token).await.0
    }
}

#[tokio::test]
async fn ssh_keys_are_released_only_to_granted_principals() {
    let Some(mut f) = Fixture::new().await else {
        return;
    };
    f.allow_ssh_targets();
    let alice = f.principal("ssh-alice").await;
    let bob = f.principal("ssh-bob").await;
    let key = f.ssh_key("ssh-alice-key", None).await;
    f.grant(Grantee::Principal(alice), "ssh_key_secret_id", key, DIRECT)
        .await;
    let alice_token = f.proxy(Some(alice), None, json!({})).await;

    let (status, grant) = f.ssh(&alice_token).await;
    assert_eq!(status, StatusCode::OK, "{grant}");
    assert_eq!(grant["id"], centaur_console_db::oid("sks", key));
    assert_eq!(grant["principal"], centaur_console_db::oid("prn", alice));
    assert_eq!(
        grant["destination"],
        "wss://one.dev.example.com/events/events"
    );
    assert_eq!(grant["username"], "dev");
    assert_eq!(grant["host_key"], "");
    assert_eq!(grant["tls_identity"], true);
    assert_eq!(
        grant["private_key"],
        json!({"type": "control_plane", "value": "known secret"})
    );
    let expiry =
        chrono::DateTime::parse_from_rfc3339(grant["expires_at"].as_str().unwrap()).unwrap();
    assert!(expiry > chrono::Utc::now());
    assert!(expiry <= chrono::Utc::now() + chrono::Duration::seconds(30));

    // No grant, no assignment, or no valid token: no key.
    let bob_token = f.proxy(Some(bob), None, json!({})).await;
    let unassigned = f.proxy(None, None, json!({})).await;
    for token in [bob_token.as_str(), unassigned.as_str(), "invalid-token"] {
        assert_eq!(f.ssh_status(token).await, StatusCode::UNAUTHORIZED);
    }

    // Revoking the grant ends access.
    f.update("DELETE FROM grants WHERE ssh_key_secret_id = $1", key)
        .await;
    assert_eq!(f.ssh_status(&alice_token).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ssh_channel_turns_use_the_channel_grant_never_the_requester() {
    let Some(mut f) = Fixture::new().await else {
        return;
    };
    f.allow_ssh_targets();
    let channel = f.principal("ssh-channel").await;
    let alice = f.principal("ssh-requester").await;
    let personal = f.ssh_key("ssh-personal-key", None).await;
    f.grant(
        Grantee::Principal(alice),
        "ssh_key_secret_id",
        personal,
        DIRECT,
    )
    .await;
    let token = f.proxy(Some(channel), Some(alice), json!({})).await;

    // The requester's own key is never hoisted into a channel turn.
    assert_eq!(f.ssh_status(&token).await, StatusCode::UNAUTHORIZED);

    // An admin grant to a role the channel holds makes a key available.
    let team = f.ssh_key("ssh-team-key", None).await;
    let role = f.role("ssh-team", channel).await;
    f.grant(Grantee::Role(role), "ssh_key_secret_id", team, ROLE)
        .await;
    let (status, grant) = f.ssh(&token).await;
    assert_eq!(status, StatusCode::OK, "{grant}");
    assert_eq!(grant["id"], centaur_console_db::oid("sks", team));
    assert_eq!(grant["principal"], centaur_console_db::oid("prn", channel));
}

#[tokio::test]
async fn ssh_grants_pick_the_strongest_key_and_refuse_ties() {
    let Some(mut f) = Fixture::new().await else {
        return;
    };
    f.allow_ssh_targets();
    let principal = f.principal("ssh-priority").await;
    let role = f.role("ssh-priority-role", principal).await;
    let shared = f.ssh_key("ssh-shared", None).await;
    let direct = f.ssh_key("ssh-direct", None).await;
    f.grant(Grantee::Role(role), "ssh_key_secret_id", shared, ROLE)
        .await;
    f.grant(
        Grantee::Principal(principal),
        "ssh_key_secret_id",
        direct,
        DIRECT,
    )
    .await;
    let token = f.proxy(Some(principal), None, json!({})).await;
    let (status, grant) = f.ssh(&token).await;
    assert_eq!(status, StatusCode::OK, "{grant}");
    assert_eq!(grant["id"], centaur_console_db::oid("sks", direct));

    f.update(
        "UPDATE grants SET priority = 100 WHERE ssh_key_secret_id = $1",
        shared,
    )
    .await;
    assert_eq!(
        f.ssh_status(&token).await,
        StatusCode::UNAUTHORIZED,
        "equally strong keys are ambiguous"
    );
}

#[tokio::test]
async fn ssh_external_sources_are_resolved_by_the_proxy() {
    let Some(mut f) = Fixture::new().await else {
        return;
    };
    f.allow_ssh_targets();
    let principal = f.principal("ssh-env").await;
    let key: i64 = sqlx::query_scalar(
        "INSERT INTO ssh_key_secrets (foreign_id, name, public_key, created_by_id, created_at, updated_at) \
         VALUES ($1, $1, 'ssh-ed25519 AAAAfixture', $2, now(), now()) RETURNING id",
    )
    .bind(f.name("ssh-env-key"))
    .bind(f.admin)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    f.source(
        "ssh_key_secret_id",
        key,
        "env",
        json!({ "var": "TEAM_SSH_KEY" }),
        None,
    )
    .await;
    f.grant(
        Grantee::Principal(principal),
        "ssh_key_secret_id",
        key,
        DIRECT,
    )
    .await;
    let token = f.proxy(Some(principal), None, json!({})).await;
    let (status, grant) = f.ssh(&token).await;
    assert_eq!(status, StatusCode::OK, "{grant}");
    assert_eq!(
        grant["private_key"],
        json!({"type": "env", "var": "TEAM_SSH_KEY"})
    );
}

#[tokio::test]
async fn ssh_leases_change_on_reassignment_and_key_replacement() {
    let Some(mut f) = Fixture::new().await else {
        return;
    };
    f.allow_ssh_targets();
    let alice = f.principal("ssh-lease-alice").await;
    let bob = f.principal("ssh-lease-bob").await;
    let key = f.ssh_key("ssh-lease-key", None).await;
    f.grant(Grantee::Principal(alice), "ssh_key_secret_id", key, DIRECT)
        .await;
    let token = f.proxy(Some(alice), None, json!({})).await;
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    let mut previous = f.ssh(&token).await.1["generation"].clone();
    assert!(previous.is_string());
    assert_eq!(f.ssh(&token).await.1["generation"], previous);

    // Reassigning away and back, even to the same principal, starts a new lease.
    for (step, other) in [Some(bob), None].into_iter().enumerate() {
        sqlx::query("UPDATE proxies SET principal_id = $1, principal_assigned_at = now() WHERE bearer_token_hash = $2")
            .bind(other).bind(&token_hash).execute(&f.pool).await.unwrap();
        assert_eq!(f.ssh_status(&token).await, StatusCode::UNAUTHORIZED);
        sqlx::query("UPDATE proxies SET principal_id = $1, principal_assigned_at = now() + make_interval(secs => $3) WHERE bearer_token_hash = $2")
            .bind(alice).bind(&token_hash).bind((step + 1) as f64).execute(&f.pool).await.unwrap();
        let (status, renewed) = f.ssh(&token).await;
        assert_eq!(status, StatusCode::OK);
        assert_ne!(renewed["generation"], previous);
        previous = renewed["generation"].clone();
    }

    // An assignment without an epoch fails closed.
    sqlx::query("UPDATE proxies SET principal_assigned_at = NULL WHERE bearer_token_hash = $1")
        .bind(&token_hash)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(f.ssh_status(&token).await, StatusCode::UNAUTHORIZED);
    sqlx::query("UPDATE proxies SET principal_assigned_at = now() + interval '10 seconds' WHERE bearer_token_hash = $1")
        .bind(&token_hash).execute(&f.pool).await.unwrap();
    previous = f.ssh(&token).await.1["generation"].clone();

    // Console touches the key whenever its source is replaced.
    f.update(
        "UPDATE ssh_key_secrets SET updated_at = now() + interval '1 second' WHERE id = $1",
        key,
    )
    .await;
    assert_ne!(f.ssh(&token).await.1["generation"], previous);
}

#[tokio::test]
async fn ssh_personal_keys_require_an_active_owner() {
    let Some(mut f) = Fixture::new().await else {
        return;
    };
    f.allow_ssh_targets();
    let owner: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, status, created_at, updated_at) VALUES ($1, 'active', now(), now()) RETURNING id",
    )
    .bind(format!("ssh-owner-{}@example.com", f.unique))
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let principal = f.principal("ssh-owned").await;
    let key = f.ssh_key("ssh-owned-key", Some(owner)).await;
    f.grant(
        Grantee::Principal(principal),
        "ssh_key_secret_id",
        key,
        DIRECT,
    )
    .await;
    let token = f.proxy(Some(principal), None, json!({})).await;
    assert_eq!(f.ssh_status(&token).await, StatusCode::OK);
    for status in ["disabled", "pending"] {
        sqlx::query("UPDATE users SET status = $1 WHERE id = $2")
            .bind(status)
            .bind(owner)
            .execute(&f.pool)
            .await
            .unwrap();
        assert_eq!(
            f.ssh_status(&token).await,
            StatusCode::UNAUTHORIZED,
            "{status}"
        );
    }
}

#[tokio::test]
async fn ssh_requests_are_limited_to_deployment_policy() {
    let Some(mut f) = Fixture::new().await else {
        return;
    };
    let principal = f.principal("ssh-policy").await;
    let key = f.ssh_key("ssh-policy-key", None).await;
    f.grant(
        Grantee::Principal(principal),
        "ssh_key_secret_id",
        key,
        DIRECT,
    )
    .await;
    let token = f.proxy(Some(principal), None, json!({})).await;
    // No configured targets denies every destination.
    assert_eq!(f.ssh_status(&token).await, StatusCode::UNAUTHORIZED);

    f.allow_ssh_targets();
    for target in [
        "wss://one.dev.example.com/events/events",
        "wss://two.dev.example.com/events/events",
    ] {
        let (status, grant) = f
            .post(SSH_ENDPOINT, Some(&token), ssh_request(target))
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(grant["destination"], target);
    }
    for target in [
        "wss://evil.example.com/events/events",
        "wss://one.dev.example.com/other",
    ] {
        assert_eq!(
            f.post(SSH_ENDPOINT, Some(&token), ssh_request(target))
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let mut wrong_route = ssh_request("wss://one.dev.example.com/events/events");
    wrong_route["route"] = json!("other");
    assert_eq!(
        f.post(SSH_ENDPOINT, Some(&token), wrong_route).await.0,
        StatusCode::UNAUTHORIZED
    );

    // The tool cannot add fields such as a requester; unknown fields are refused.
    for body in [
        json!({"route": "ee", "public_key": "ssh-ed25519 sandbox-key"}),
        json!({
            "route": "ee",
            "public_key": "ssh-ed25519 sandbox-key",
            "destination": "wss://one.dev.example.com/events/events",
            "requester_principal_id": centaur_console_db::oid("prn", principal),
        }),
    ] {
        let response = router(f.state.clone())
            .oneshot(
                Request::post(SSH_ENDPOINT)
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[tokio::test]
async fn disabled_apps_keep_existing_credentials_available() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let principal = f.principal("disabled-app-owner").await;
    let app = f.oauth_app("disabled-github", true).await;
    let credential = f.broker_credential("existing", Some(app), true).await;
    let secret = f
        .wrapper_secret("existing", credential, credential, "api.github.test")
        .await;
    f.grant(
        Grantee::Principal(principal),
        "static_secret_id",
        secret,
        DIRECT,
    )
    .await;
    sqlx::query("UPDATE oauth_apps SET enabled = false WHERE id = $1")
        .bind(app)
        .execute(&f.pool)
        .await
        .unwrap();

    // Both the live DM/requester paths and api-rs' redacted reader must retain
    // tokens for existing connections when only new consent is disabled.
    let conversation = f.principal("disabled-app-conversation").await;
    for (owner, requester) in [
        (Some(principal), None),
        (Some(conversation), Some(principal)),
    ] {
        let proxy = f.proxy(owner, requester, json!({})).await;
        let config = f.sync(&proxy).await;
        assert_eq!(secret_hosts(&config), vec!["api.github.test"]);
        assert_eq!(config["secrets"][0]["source"]["value"], "known secret");
    }
    let redacted = centaur_console_db::effective_config(&f.pool, principal)
        .await
        .unwrap()
        .unwrap();
    let redacted = json!({ "secrets": redacted.secrets });
    assert_eq!(secret_hosts(&redacted), vec!["api.github.test"]);
    assert_eq!(redacted["secrets"][0]["source"]["value"], "[redacted]");

    // The owner-only liveness gate does not change delivery for other apps.
    sqlx::query(
        "UPDATE broker_credentials SET expires_at = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(credential)
    .execute(&f.pool)
    .await
    .unwrap();
    let proxy = f.proxy(Some(principal), None, json!({})).await;
    assert_eq!(
        f.sync(&proxy).await["secrets"][0]["source"]["value"],
        "known secret"
    );
}

#[tokio::test]
async fn owner_only_credentials_follow_the_owner_and_stop_after_revocation() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    sqlx::query("UPDATE users SET status = 'active' WHERE id = $1")
        .bind(f.admin)
        .execute(&f.pool)
        .await
        .unwrap();
    let bob_owner: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, status, created_at, updated_at) VALUES ($1, 'active', now(), now()) RETURNING id"
    ).bind(format!("bob-{}@example.test", f.unique)).fetch_one(&f.pool).await.unwrap();
    let channel = f.principal("channel").await;
    let alice = f.principal("alice").await;
    let bob = f.principal("bob").await;
    for (actor, owner) in [(alice, f.admin), (bob, bob_owner)] {
        sqlx::query(
            "UPDATE principals SET kind = 'console_user', console_user_id = $1 WHERE id = $2",
        )
        .bind(owner)
        .bind(actor)
        .execute(&f.pool)
        .await
        .unwrap();
    }
    let app = f.oauth_app("personal-mcp", true).await;
    sqlx::query("UPDATE oauth_apps SET owner_only = true WHERE id = $1")
        .bind(app)
        .execute(&f.pool)
        .await
        .unwrap();
    let a = f.broker_credential("alice", Some(app), true).await;
    let b = f.broker_credential("bob", Some(app), true).await;
    sqlx::query("UPDATE broker_credentials SET created_by_id = $1 WHERE id = $2")
        .bind(bob_owner)
        .bind(b)
        .execute(&f.pool)
        .await
        .unwrap();
    let sa = f.wrapper_secret("alice", a, a, "alice.mcp.test").await;
    let sb = f.wrapper_secret("bob", b, b, "bob.mcp.test").await;
    f.grant(Grantee::Principal(alice), "static_secret_id", sa, DIRECT)
        .await;
    f.grant(Grantee::Principal(bob), "static_secret_id", sb, DIRECT)
        .await;
    // Even stale or manually inserted shared/cross-owner grants cannot supply
    // another user's owner-only token.
    let shared_role = f.role("shared", channel).await;
    f.grant(
        Grantee::Role(shared_role),
        "static_secret_id",
        sa,
        DIRECT + 100,
    )
    .await;
    f.grant(
        Grantee::Principal(bob),
        "static_secret_id",
        sa,
        DIRECT + 100,
    )
    .await;
    let token = f.proxy(Some(channel), Some(alice), json!({})).await;
    let mut last_hash = Value::Null;
    for (actor, expected) in [
        (Some(alice), vec!["alice.mcp.test"]),
        (Some(bob), vec!["bob.mcp.test"]),
        (Some(alice), vec!["alice.mcp.test"]),
        (None, vec![]),
    ] {
        sqlx::query("UPDATE proxies SET requester_principal_id = $1 WHERE bearer_token_hash = $2")
            .bind(actor)
            .bind(hex::encode(Sha256::digest(token.as_bytes())))
            .execute(&f.pool)
            .await
            .unwrap();
        let body = f.sync(&token).await;
        assert_eq!(secret_hosts(&body), expected);
        assert_ne!(body["config_hash"], last_hash);
        last_hash = body["config_hash"].clone();
    }
    // The shared reader also protects api-rs' redacted effective-config path.
    let redacted = centaur_console_db::effective_config(&f.pool, bob)
        .await
        .unwrap()
        .unwrap();
    let redacted = json!({ "secrets": redacted.secrets });
    assert_eq!(secret_hosts(&redacted), vec!["bob.mcp.test"]);
    assert_eq!(redacted["secrets"][0]["source"]["value"], "[redacted]");

    // The DM path has no requester and would otherwise cache the old token.
    let dm = f.proxy(Some(alice), None, json!({})).await;
    let previous = f.sync(&dm).await;
    assert_eq!(secret_hosts(&previous), vec!["alice.mcp.test"]);
    sqlx::query("UPDATE secret_sources SET broker_credential_id = $1 WHERE static_secret_id = $2")
        .bind(b)
        .bind(sa)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(secret_hosts(&f.sync(&dm).await).is_empty());
    sqlx::query("UPDATE secret_sources SET broker_credential_id = $1 WHERE static_secret_id = $2")
        .bind(a)
        .bind(sa)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE broker_credentials SET dead = true WHERE id = $1")
        .bind(a)
        .execute(&f.pool)
        .await
        .unwrap();
    let (status, after_revoke) = f
        .post(
            "/api/v1/proxy/sync",
            Some(&dm),
            json!({ "config_hash": previous["config_hash"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(secret_hosts(&after_revoke).is_empty());
    sqlx::query("UPDATE broker_credentials SET dead = false, expires_at = now() - interval '1 minute' WHERE id = $1")
        .bind(a).execute(&f.pool).await.unwrap();
    assert!(secret_hosts(&f.sync(&dm).await).is_empty());
    sqlx::query(
        "UPDATE broker_credentials SET expires_at = now() + interval '1 hour' WHERE id = $1",
    )
    .bind(a)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE oauth_apps SET enabled = false WHERE id = $1")
        .bind(app)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(secret_hosts(&f.sync(&dm).await).is_empty());
}

#[tokio::test]
async fn owner_only_requires_the_owners_verified_slack_team_and_active_account() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    sqlx::query("UPDATE users SET status = 'active' WHERE id = $1")
        .bind(f.admin)
        .execute(&f.pool)
        .await
        .unwrap();
    let channel = f.principal("channel").await;
    let actor = f.principal("slack-user").await;
    let subject = format!("U{}", f.unique.to_uppercase());
    sqlx::query("INSERT INTO user_identities (user_id, provider, subject, team_id, created_at, updated_at) VALUES ($1, 'slack', $2, 'T000000001', now(), now())")
        .bind(f.admin).bind(&subject).execute(&f.pool).await.unwrap();
    sqlx::query(
        "UPDATE principals SET slack_user_id = $1, slack_team_id = 'T000000001' WHERE id = $2",
    )
    .bind(&subject)
    .bind(actor)
    .execute(&f.pool)
    .await
    .unwrap();
    let app = f.oauth_app("personal-mcp-slack", true).await;
    sqlx::query("UPDATE oauth_apps SET owner_only = true WHERE id = $1")
        .bind(app)
        .execute(&f.pool)
        .await
        .unwrap();
    let credential = f.broker_credential("slack-user", Some(app), true).await;
    let secret = f
        .wrapper_secret("slack-user", credential, credential, "tenant.mcp.test")
        .await;
    f.grant(
        Grantee::Principal(actor),
        "static_secret_id",
        secret,
        DIRECT,
    )
    .await;
    let token = f.proxy(Some(channel), Some(actor), json!({})).await;
    assert_eq!(secret_hosts(&f.sync(&token).await), vec!["tenant.mcp.test"]);

    sqlx::query("UPDATE principals SET slack_team_id = 'T000000002' WHERE id = $1")
        .bind(actor)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(secret_hosts(&f.sync(&token).await).is_empty());
    sqlx::query("UPDATE principals SET slack_team_id = 'T000000001' WHERE id = $1")
        .bind(actor)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
        .bind(f.admin)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(secret_hosts(&f.sync(&token).await).is_empty());
    sqlx::query("UPDATE users SET status = 'active' WHERE id = $1")
        .bind(f.admin)
        .execute(&f.pool)
        .await
        .unwrap();

    // A Console principal cannot borrow another owner's Slack identity.
    sqlx::query(
        "UPDATE principals SET kind = 'console_user', console_user_id = NULL WHERE id = $1",
    )
    .bind(actor)
    .execute(&f.pool)
    .await
    .unwrap();
    assert!(secret_hosts(&f.sync(&token).await).is_empty());
    sqlx::query("UPDATE principals SET kind = 'user' WHERE id = $1")
        .bind(actor)
        .execute(&f.pool)
        .await
        .unwrap();
    // Console ignores identities without a team when resolving the owner's
    // Slack identity, so they must not make the owner ambiguous here either.
    sqlx::query("INSERT INTO user_identities (user_id, provider, subject, team_id, created_at, updated_at) VALUES ($1, 'slack', $2, NULL, now(), now())")
        .bind(f.admin).bind(format!("UNOTEAM{}", f.unique.to_uppercase())).execute(&f.pool).await.unwrap();
    assert_eq!(secret_hosts(&f.sync(&token).await), vec!["tenant.mcp.test"]);
    sqlx::query("INSERT INTO user_identities (user_id, provider, subject, team_id, created_at, updated_at) VALUES ($1, 'slack', $2, 'T000000002', now(), now())")
        .bind(f.admin).bind(format!("W{}", f.unique.to_uppercase())).execute(&f.pool).await.unwrap();
    assert!(secret_hosts(&f.sync(&token).await).is_empty());
}
