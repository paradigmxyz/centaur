//! Refreshes against a Postgres database holding Console's schema and a local
//! token endpoint.
//!
//! Set `TOKEN_BROKER_TEST_DATABASE_URL` to a database prepared with Console's
//! `bin/rails db:schema:load`. Each test refreshes only the rows it inserts,
//! so tests can share one database and run in parallel.

use std::{
    collections::VecDeque,
    env,
    sync::{Arc, Mutex},
};

use active_record_encryption::ActiveRecordEncryption;
use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    routing::post,
};
use chrono::{NaiveDateTime, TimeDelta, Utc};
use metrics_exporter_prometheus::PrometheusBuilder;
use serde_json::{Value, json};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};

use crate::{broker::Broker, client::TokenClient, store};

const PRIMARY_KEY: &str = "dev_ar_encryption_primary_key_0000000000000000";
const SALT: &str = "dev_ar_encryption_key_derivation_salt_000000000";

/// A token endpoint, and its sibling `refresh_token` endpoint, answering with
/// queued responses in order and recording the requests.
#[derive(Default)]
struct Endpoint {
    responses: Mutex<VecDeque<(u16, String)>>,
    requests: Mutex<Vec<Request>>,
}

impl Endpoint {
    async fn start(responses: &[(u16, Value)]) -> (String, Arc<Self>) {
        let endpoint = Arc::new(Self::default());
        endpoint.responses.lock().unwrap().extend(
            responses
                .iter()
                .map(|(status, body)| (*status, body.to_string())),
        );
        let handler = post(
            |State(endpoint): State<Arc<Self>>, uri: Uri, headers: HeaderMap, body: String| async move {
                endpoint.requests.lock().unwrap().push(Request {
                    path: uri.path().to_owned(),
                    headers,
                    body,
                });
                let (status, body) = endpoint
                    .responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected token request");
                (StatusCode::from_u16(status).unwrap(), body)
            },
        );
        let app = Router::new()
            .route("/token", handler.clone())
            .route("/refresh_token", handler)
            .with_state(endpoint.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        (url, endpoint)
    }
}

struct Request {
    path: String,
    headers: HeaderMap,
    body: String,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    fn form(&self) -> Vec<(String, String)> {
        url::form_urlencoded::parse(self.body.as_bytes())
            .into_owned()
            .collect()
    }

    fn multipart_field(&self, name: &str) -> Option<&str> {
        let start = format!("name=\"{name}\"\r\n\r\n");
        let value = &self.body[self.body.find(&start)? + start.len()..];
        value.split("\r\n").next()
    }
}

fn form(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// A credential's stored state, decrypted, with times in seconds from now.
#[derive(Debug)]
struct Stored {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    next_attempt_in: Option<i64>,
    failure_count: i32,
    dead: bool,
    dead_reason: Option<String>,
}

struct Fixture {
    pool: PgPool,
    broker: Broker,
    encryption: Arc<ActiveRecordEncryption>,
    endpoint: Arc<Endpoint>,
    url: String,
    admin: i64,
    unique: String,
}

impl Fixture {
    /// Connects to the test database, or returns `None` to skip outside CI,
    /// and starts a token endpoint answering with `responses`.
    async fn new(responses: &[(u16, Value)]) -> Option<Self> {
        let Ok(database_url) = env::var("TOKEN_BROKER_TEST_DATABASE_URL") else {
            assert!(
                env::var_os("CI").is_none(),
                "TOKEN_BROKER_TEST_DATABASE_URL must be set in CI"
            );
            eprintln!(
                "skipping: set TOKEN_BROKER_TEST_DATABASE_URL to a database with Console's schema"
            );
            return None;
        };
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await
            .expect("connect to the test database");
        let unique = format!("{:032x}", rand::random::<u128>());
        let admin = sqlx::query_scalar(
            "INSERT INTO users (email, admin, status, created_at, updated_at) \
             VALUES ($1, true, 'approved', now(), now()) RETURNING id",
        )
        .bind(format!("admin-{unique}@example.com"))
        .fetch_one(&pool)
        .await
        .unwrap();
        let encryption = Arc::new(ActiveRecordEncryption::new(PRIMARY_KEY, SALT));
        let (url, endpoint) = Endpoint::start(responses).await;
        Some(Self {
            broker: Broker {
                pool: pool.clone(),
                encryption: encryption.clone(),
                client: TokenClient::new().unwrap(),
                concurrency: 1,
            },
            pool,
            encryption,
            endpoint,
            url,
            admin,
            unique,
        })
    }

    fn name(&self, label: &str) -> String {
        format!("{label}-{}-{}", self.unique, rand::random::<u32>())
    }

    /// A due credential for the token endpoint, with client `cid`, scopes
    /// `a b`, and the given encrypted columns.
    async fn credential(&self, grant: &str, encrypted: &[(&str, &str)]) -> i64 {
        let id = sqlx::query_scalar(
            "INSERT INTO broker_credentials (foreign_id, \"grant\", token_endpoint, client_id, \
                 scopes, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, $3, 'cid', '[\"a\", \"b\"]', $4, now(), now()) RETURNING id",
        )
        .bind(self.name(grant))
        .bind(grant)
        .bind(&self.url)
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        for (column, value) in encrypted {
            sqlx::query(&format!(
                "UPDATE broker_credentials SET {column} = $2 WHERE id = $1"
            ))
            .bind(id)
            .bind(self.encryption.encrypt(value).unwrap())
            .execute(&self.pool)
            .await
            .unwrap();
        }
        id
    }

    /// Links `credential` to a new OAuth app for `provider`.
    async fn link_oauth_app(&self, credential: i64, provider: &str, client_id: &str, secret: &str) {
        let app: i64 = sqlx::query_scalar(
            "INSERT INTO oauth_apps (slug, provider, client_id, client_secret, created_by_id, \
                 created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, now(), now()) RETURNING id",
        )
        .bind(self.name(provider))
        .bind(provider)
        .bind(client_id)
        .bind(self.encryption.encrypt(secret).unwrap())
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        self.update(credential, &format!("oauth_app_id = {app}"))
            .await;
    }

    async fn update(&self, credential: i64, assignments: &str) {
        sqlx::query(&format!(
            "UPDATE broker_credentials SET {assignments} WHERE id = $1"
        ))
        .bind(credential)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn principal(&self) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO principals (foreign_id, kind, created_by_id, created_at, updated_at) \
             VALUES ($1, 'user', $2, now(), now()) RETURNING id",
        )
        .bind(self.name("principal"))
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// Delivers `credential`'s token through a static secret granted to one
    /// new principal directly and to another through a role; returns both.
    async fn grantees(&self, credential: i64) -> [i64; 2] {
        let [direct, member] = [self.principal().await, self.principal().await];
        sqlx::query(
            "WITH secret AS ( \
                 INSERT INTO static_secrets (foreign_id, inject_config, created_by_id, created_at, updated_at) \
                 VALUES ($1, '{\"header\": \"Authorization\"}', $5, now(), now()) RETURNING id \
             ), source AS ( \
                 INSERT INTO secret_sources (static_secret_id, source_type, broker_credential_id, created_at, updated_at) \
                 SELECT id, 'token_broker', $2, now(), now() FROM secret RETURNING id \
             ), role AS ( \
                 INSERT INTO roles (foreign_id, created_by_id, created_at, updated_at) \
                 VALUES ($1, $5, now(), now()) RETURNING id \
             ), membership AS ( \
                 INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) \
                 SELECT $4, id, now(), now() FROM role \
             ) \
             INSERT INTO grants (principal_id, role_id, static_secret_id, priority, created_by_id, created_at, updated_at) \
             SELECT $3, NULL, secret.id, 100, $5, now(), now() FROM secret \
             UNION ALL SELECT NULL, role.id, secret.id, 0, $5, now(), now() FROM secret, role",
        )
        .bind(self.name("grantees"))
        .bind(credential)
        .bind(direct)
        .bind(member)
        .bind(self.admin)
        .execute(&self.pool)
        .await
        .unwrap();
        [direct, member]
    }

    async fn cache_versions(&self, principals: &[i64]) -> Vec<i64> {
        sqlx::query_scalar(
            "SELECT sync_config_cache_version FROM unnest($1::bigint[]) WITH ORDINALITY AS ids (id, n) \
             JOIN principals USING (id) ORDER BY n",
        )
        .bind(principals)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    /// The requests the token endpoint received since the last call.
    fn requests(&self) -> Vec<Request> {
        std::mem::take(&mut self.endpoint.requests.lock().unwrap())
    }

    /// Refreshes `credential` and returns what it stored.
    async fn refresh(&self, credential: i64) -> Stored {
        self.broker.refresh(credential).await;
        self.stored(credential).await
    }

    async fn stored(&self, credential: i64) -> Stored {
        let row = sqlx::query(
            "SELECT access_token, refresh_token, expires_at, next_attempt_at, failure_count, \
                 dead, dead_reason \
             FROM broker_credentials WHERE id = $1",
        )
        .bind(credential)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        let decrypt = |column: &str| {
            row.get::<Option<String>, _>(column)
                .map(|value| self.encryption.decrypt(&value).unwrap())
        };
        let seconds_from_now = |column: &str| {
            row.get::<Option<NaiveDateTime>, _>(column)
                .map(|time| (time - Utc::now().naive_utc()).num_seconds())
        };
        Stored {
            access_token: decrypt("access_token"),
            refresh_token: decrypt("refresh_token"),
            expires_in: seconds_from_now("expires_at"),
            next_attempt_in: seconds_from_now("next_attempt_at"),
            failure_count: row.get("failure_count"),
            dead: row.get("dead"),
            dead_reason: row.get("dead_reason"),
        }
    }
}

/// A fixture whose token endpoint answers with the given responses; the test
/// returns early when no test database is configured.
macro_rules! fixture {
    ($($response:expr),* $(,)?) => {
        match Fixture::new(&[$($response),*]).await {
            Some(fixture) => fixture,
            None => return,
        }
    };
}

fn within(seconds: Option<i64>, expected: i64) -> bool {
    seconds.is_some_and(|seconds| (expected - 5..=expected).contains(&seconds))
}

#[tokio::test]
async fn refresh_rotates_tokens_and_invalidates_grantee_proxy_configs() {
    let fixture = fixture![(
        200,
        json!({"access_token": "AT-1", "refresh_token": "RT-2", "expires_in": 3600}),
    )];
    let id = fixture
        .credential(
            "refresh_token",
            &[
                ("refresh_token", "RT-1"),
                ("client_secret", "secret"),
                ("token_endpoint_headers", r#"{"X-Api-Key":"k"}"#),
            ],
        )
        .await;
    let [direct, member] = fixture.grantees(id).await;
    let unrelated = fixture.principal().await;

    let stored = fixture.refresh(id).await;

    let request = &fixture.requests()[0];
    assert_eq!(
        request.form(),
        form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", "RT-1"),
            ("client_id", "cid"),
            ("client_secret", "secret"),
            ("scope", "a b"),
        ])
    );
    assert_eq!(request.header("x-api-key"), Some("k"));
    assert_eq!(request.header("accept"), Some("application/json"));
    assert_eq!(stored.access_token.as_deref(), Some("AT-1"));
    assert_eq!(stored.refresh_token.as_deref(), Some("RT-2"));
    assert!(within(stored.expires_in, 3600));
    // 3600s minus the larger of the 300s slack and 20% of the lifetime.
    assert!(within(stored.next_attempt_in, 2880));
    assert_eq!((stored.failure_count, stored.dead), (0, false));
    assert_eq!(
        fixture.cache_versions(&[direct, member, unrelated]).await,
        [1, 1, 0]
    );

    // The next refresh is not due yet.
    fixture.refresh(id).await;
    assert!(fixture.requests().is_empty());
}

#[tokio::test]
async fn refresh_keeps_an_unrotated_refresh_token_and_defaults_the_expiry() {
    let fixture = fixture![(200, json!({"access_token": "AT"}))];
    let id = fixture
        .credential("refresh_token", &[("refresh_token", "RT-keep")])
        .await;

    let stored = fixture.refresh(id).await;

    assert_eq!(stored.access_token.as_deref(), Some("AT"));
    assert_eq!(stored.refresh_token.as_deref(), Some("RT-keep"));
    assert!(within(stored.expires_in, 300));
}

#[tokio::test]
async fn transient_failures_back_off_and_rejections_mark_the_credential_dead() {
    let fixture = fixture![
        (503, json!("upstream down")),
        (400, json!({"error": "invalid_grant"}))
    ];
    let id = fixture
        .credential("refresh_token", &[("refresh_token", "RT")])
        .await;
    let [direct, _] = fixture.grantees(id).await;

    let stored = fixture.refresh(id).await;
    assert_eq!((stored.failure_count, stored.dead), (1, false));
    assert!(within(stored.next_attempt_in, 5));
    assert_eq!(fixture.cache_versions(&[direct]).await, [0]);

    fixture.update(id, "next_attempt_at = now()").await;
    let stored = fixture.refresh(id).await;
    assert!(stored.dead);
    assert_eq!(stored.dead_reason.as_deref(), Some("invalid_grant"));
    assert_eq!(stored.refresh_token.as_deref(), Some("RT"));
    assert_eq!(fixture.cache_versions(&[direct]).await, [1]);
}

#[tokio::test]
async fn password_grant_replaces_a_rejected_refresh_token() {
    let fixture = fixture![
        (400, json!({"error": "invalid_grant"})),
        (200, json!({"access_token": "AT-password"}))
    ];
    let id = fixture
        .credential(
            "password",
            &[
                ("refresh_token", "RT-stale"),
                ("username", "user"),
                ("password", "pass"),
            ],
        )
        .await;

    let stored = fixture.refresh(id).await;

    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].form(),
        form(&[
            ("grant_type", "password"),
            ("username", "user"),
            ("password", "pass"),
            ("client_id", "cid"),
            ("scope", "a b"),
        ])
    );
    assert_eq!(stored.access_token.as_deref(), Some("AT-password"));
    assert_eq!(stored.refresh_token, None);
    assert!(!stored.dead);
}

#[tokio::test]
async fn password_grant_keeps_its_refresh_token_through_transient_failures() {
    let fixture = fixture![(503, json!("upstream down"))];
    let id = fixture
        .credential(
            "password",
            &[
                ("refresh_token", "RT"),
                ("username", "user"),
                ("password", "pass"),
            ],
        )
        .await;

    let stored = fixture.refresh(id).await;

    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].form()[..1],
        form(&[("grant_type", "refresh_token")])
    );
    assert_eq!(stored.refresh_token.as_deref(), Some("RT"));
    assert_eq!((stored.failure_count, stored.dead), (1, false));
}

#[tokio::test]
async fn preqin_grant_replaces_a_refresh_token_its_refresh_endpoint_rejects() {
    // A bare 4xx is a rejection from Preqin.
    let fixture = fixture![
        (400, json!("")),
        (
            200,
            json!({"access_token": "AT-preqin", "refresh_token": "RT-good"}),
        )
    ];
    let recorder = PrometheusBuilder::new().build_recorder();
    let metrics = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let id = fixture
        .credential(
            "preqin",
            &[
                ("refresh_token", "RT-bad"),
                ("username", "user"),
                ("api_key", "api-key"),
            ],
        )
        .await;

    let stored = fixture.refresh(id).await;

    let requests = fixture.requests();
    let [refresh, initial] = &requests[..] else {
        panic!("expected two token requests");
    };
    assert_eq!(refresh.path, "/refresh_token");
    assert_eq!(refresh.multipart_field("refresh_token"), Some("RT-bad"));
    assert_eq!(initial.path, "/token");
    assert!(
        initial
            .header("content-type")
            .is_some_and(|value| value.starts_with("multipart/form-data"))
    );
    assert_eq!(initial.multipart_field("username"), Some("user"));
    assert_eq!(initial.multipart_field("apikey"), Some("api-key"));
    assert_eq!(initial.multipart_field("client_id"), None);
    assert!(metrics.render().contains(
        r#"token_broker_token_requests_total{provider="127.0.0.1",result="rejected"} 1"#
    ));
    assert_eq!(stored.access_token.as_deref(), Some("AT-preqin"));
    assert_eq!(stored.refresh_token.as_deref(), Some("RT-good"));
    assert!(!stored.dead);
}

#[tokio::test]
async fn client_credentials_grant_requests_a_token_for_the_client() {
    let fixture = fixture![(200, json!({"access_token": "AT", "expires_in": 7199}))];
    let id = fixture
        .credential("client_credentials", &[("client_secret", "secret")])
        .await;

    let stored = fixture.refresh(id).await;

    assert_eq!(
        fixture.requests()[0].form(),
        form(&[
            ("grant_type", "client_credentials"),
            ("client_id", "cid"),
            ("client_secret", "secret"),
            ("scope", "a b"),
        ])
    );
    assert_eq!(stored.access_token.as_deref(), Some("AT"));
    assert_eq!(stored.refresh_token, None);
    assert!(within(stored.expires_in, 7199));
}

#[tokio::test]
async fn app_credentials_refresh_with_their_providers_client_authentication() {
    let fixture = fixture![(200, json!({"access_token": "AT"}))];
    let id = fixture
        .credential("refresh_token", &[("refresh_token", "RT")])
        .await;
    let unseeded = fixture.credential("refresh_token", &[]).await;
    for credential in [id, unseeded] {
        fixture
            .link_oauth_app(credential, "zoom", "app:id", "secret+value")
            .await;
    }

    fixture.refresh(id).await;
    let unseeded = fixture.refresh(unseeded).await;

    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    // The app's client, form-encoded, in HTTP Basic auth; Zoom refreshes
    // without scopes.
    assert_eq!(
        requests[0].header("authorization"),
        Some("Basic YXBwJTNBaWQ6c2VjcmV0JTJCdmFsdWU=")
    );
    assert_eq!(
        requests[0].form(),
        form(&[("grant_type", "refresh_token"), ("refresh_token", "RT")])
    );
    assert!(unseeded.dead);
    assert_eq!(
        unseeded.dead_reason.as_deref(),
        Some("missing_initial_refresh_token")
    );
}

#[tokio::test]
async fn credentials_that_cannot_build_a_request_back_off() {
    let fixture = fixture![];
    let unknown_provider = fixture
        .credential("refresh_token", &[("refresh_token", "RT")])
        .await;
    fixture
        .link_oauth_app(unknown_provider, "unlisted", "app-id", "secret")
        .await;
    let missing_secret = fixture.credential("client_credentials", &[]).await;

    for id in [unknown_provider, missing_secret] {
        let stored = fixture.refresh(id).await;
        assert_eq!((stored.failure_count, stored.dead), (1, false));
        assert!(within(stored.next_attempt_in, 5));
    }
    assert!(fixture.requests().is_empty());
}

#[tokio::test]
async fn metrics_count_refreshes_and_token_requests_by_provider() {
    let fixture = fixture![
        (429, json!("slow down")),
        (200, json!({"access_token": "AT"}))
    ];
    let recorder = PrometheusBuilder::new().build_recorder();
    let metrics = recorder.handle();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let id = fixture
        .credential("refresh_token", &[("refresh_token", "RT")])
        .await;
    fixture.link_oauth_app(id, "google", "cid", "secret").await;

    fixture.refresh(id).await;
    fixture.update(id, "next_attempt_at = now()").await;
    fixture.refresh(id).await;

    let metrics = metrics.render();
    for line in [
        r#"token_broker_token_requests_total{provider="google",result="rate_limited"} 1"#,
        r#"token_broker_token_requests_total{provider="google",result="success"} 1"#,
        r#"token_broker_refreshes_total{provider="google",grant="refresh_token",outcome="retry"} 1"#,
        r#"token_broker_refreshes_total{provider="google",grant="refresh_token",outcome="success"} 1"#,
    ] {
        assert!(metrics.contains(line), "{line} missing from:\n{metrics}");
    }
}

#[tokio::test]
async fn credential_counts_group_dead_and_backing_off_credentials_by_provider() {
    let fixture = fixture![];
    let provider = fixture.name("provider");
    for update in ["dead = TRUE", "failure_count = 2", "failure_count = 0"] {
        let id = fixture
            .credential("refresh_token", &[("refresh_token", "RT")])
            .await;
        fixture.link_oauth_app(id, &provider, "cid", "secret").await;
        fixture.update(id, update).await;
    }

    let counts: Vec<(i64, i64)> = store::credential_counts(&fixture.pool)
        .await
        .unwrap()
        .into_iter()
        .filter(|counts| counts.provider.as_deref() == Some(&provider))
        .map(|counts| (counts.dead, counts.backing_off))
        .collect();

    assert_eq!(counts, [(1, 1)]);
}

#[tokio::test]
async fn refresh_skips_a_credential_whose_row_is_locked() {
    let fixture = fixture![];
    let id = fixture
        .credential("refresh_token", &[("refresh_token", "RT")])
        .await;
    let mut lock = fixture.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM broker_credentials WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut *lock)
        .await
        .unwrap();

    fixture.broker.refresh(id).await;

    assert!(fixture.requests().is_empty());
    lock.rollback().await.unwrap();
    assert_eq!(fixture.stored(id).await.failure_count, 0);
}

#[tokio::test]
async fn due_credentials_follow_the_refreshable_scope() {
    let fixture = fixture![];
    let mut credentials = Vec::new();
    for (grant, encrypted, update) in [
        ("refresh_token", &[("refresh_token", "RT")][..], None),
        (
            "password",
            &[("username", "u"), ("password", "p")],
            Some("last_refresh = now(), next_attempt_at = now() - interval '1 minute'"),
        ),
        (
            "refresh_token",
            &[("refresh_token", "RT")],
            Some("next_attempt_at = now() + interval '1 hour'"),
        ),
        (
            "refresh_token",
            &[("refresh_token", "RT")],
            Some("dead = TRUE"),
        ),
        // Refreshed once, but issued no refresh token to refresh with.
        ("refresh_token", &[], Some("last_refresh = now()")),
    ] {
        let id = fixture.credential(grant, encrypted).await;
        if let Some(update) = update {
            fixture.update(id, update).await;
        }
        credentials.push(id);
    }

    let due = store::due_credential_ids(
        &fixture.pool,
        Utc::now().naive_utc() + TimeDelta::seconds(1),
    )
    .await
    .unwrap();

    let due: Vec<bool> = credentials.iter().map(|id| due.contains(id)).collect();
    assert_eq!(due, [true, true, false, false, false]);
}
