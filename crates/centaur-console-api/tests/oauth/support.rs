//! A stub upstream (IdPs, provider APIs), an app wired to it, and fixtures.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{Router, body::Bytes, extract::State, http::Uri};
use centaur_console_api::{
    oauth,
    testing::{TestApp, TestUser},
};
use futures_util::future::BoxFuture;
use http::{HeaderMap, Method, StatusCode};
use serde_json::{Value, json};

/// One request the stub saw.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: Method,
    /// `{host}{path}`.
    pub target: String,
    pub query: Option<String>,
    pub authorization: Option<String>,
    pub body: String,
}

impl Seen {
    pub fn form(&self) -> Vec<(String, String)> {
        url::form_urlencoded::parse(self.body.as_bytes())
            .into_owned()
            .collect()
    }

    pub fn form_value(&self, key: &str) -> Option<String> {
        self.form()
            .into_iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

/// Status, content type, and body of one stubbed answer.
type Answer = (u16, String, String);

/// Work to do while a request is in flight, before it is answered.
type InFlight = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Clone, Default)]
pub struct Stub {
    answers: Arc<Mutex<HashMap<String, Answer>>>,
    in_flight: Arc<Mutex<HashMap<String, InFlight>>>,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

impl Stub {
    /// Answers `target` (`{host}{path}`) with a JSON body.
    pub fn answer(&self, target: &str, status: u16, body: &Value) {
        self.answers.lock().unwrap().insert(
            target.to_owned(),
            (status, "application/json".into(), body.to_string()),
        );
    }

    /// Runs `work` after the request for `target` arrived and before it is
    /// answered: something else happening during the upstream call.
    pub fn meanwhile(
        &self,
        target: &str,
        work: impl Fn() -> BoxFuture<'static, ()> + Send + Sync + 'static,
    ) {
        self.in_flight
            .lock()
            .unwrap()
            .insert(target.to_owned(), Arc::new(work));
    }

    pub fn answer_raw(&self, target: &str, status: u16, content_type: &str, body: &str) {
        self.answers.lock().unwrap().insert(
            target.to_owned(),
            (status, content_type.into(), body.to_owned()),
        );
    }

    pub fn requests_to(&self, target: &str) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| seen.target == target)
            .cloned()
            .collect()
    }
}

async fn handle(
    State(stub): State<Stub>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, [(&'static str, String); 1], String) {
    let target = uri.path().trim_start_matches('/').to_owned();
    stub.seen.lock().unwrap().push(Seen {
        method,
        target: target.clone(),
        query: uri.query().map(str::to_owned),
        authorization: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    let work = stub.in_flight.lock().unwrap().get(&target).cloned();
    if let Some(work) = work {
        work().await;
    }
    let (status, content_type, body) = stub
        .answers
        .lock()
        .unwrap()
        .get(&target)
        .cloned()
        .unwrap_or((404, "application/json".into(), "{}".into()));
    (
        StatusCode::from_u16(status).unwrap(),
        [("content-type", content_type)],
        body,
    )
}

/// Starts the stub; returns it and its base URL.
pub async fn stub() -> (Stub, String) {
    let stub = Stub::default();
    let router = Router::new().fallback(handle).with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (stub, base)
}

pub struct Fixture {
    pub app: TestApp,
    pub stub: Stub,
    pub admin: TestUser,
    pub member: TestUser,
}

pub const MCP_RESOURCE: &str = "https://console.example.test/mcp";

/// The oauth routes over a database, wired to a stub upstream; one admin and
/// one member, both active. `None` without a database.
pub async fn fixture() -> Option<Fixture> {
    let (stub, base) = stub().await;
    let app = TestApp::with_env(
        oauth::routes(),
        &[
            ("CENTAUR_CONSOLE_OAUTH_TEST_BASE_URL", base.as_str()),
            ("CENTAUR_MCP_PUBLIC_URL", MCP_RESOURCE),
        ],
    )
    .await?;
    Some(complete(app, stub).await)
}

async fn complete(app: TestApp, stub: Stub) -> Fixture {
    let admin = app.create_user("admin@example.test", true, "active").await;
    let member = app
        .create_user("member@example.test", false, "active")
        .await;
    Fixture {
        app,
        stub,
        admin,
        member,
    }
}

impl Fixture {
    pub async fn admin_key(&self) -> String {
        self.app.create_api_key(self.admin.id).await
    }

    pub fn browser(&self, user: &TestUser) -> centaur_console_api::testing::TestClient {
        let mut browser = self.app.client();
        browser.sign_in(user.id);
        browser
    }

    /// An OAuth app straight into the database (no API involved).
    pub async fn insert_app(&self, slug: &str, provider: &str, shared: bool) -> i64 {
        let secret = centaur_console_db::Encryptor::development()
            .encrypt("app-secret")
            .unwrap();
        sqlx::query_scalar(
            "INSERT INTO oauth_apps (slug, provider, client_id, client_secret, allowed_scopes, shared, \
             created_by_id, created_at, updated_at) VALUES ($1, $2, 'client-id', $3, $4, $5, $6, $7, $7) \
             RETURNING id",
        )
        .bind(slug)
        .bind(provider)
        .bind(secret)
        .bind(json!(["scope-a", "scope-b"]))
        .bind(shared)
        .bind(self.admin.id)
        .bind(self.app.now().naive_utc())
        .fetch_one(&self.app.db.pool)
        .await
        .unwrap()
    }

    pub async fn version(&self, principal_id: i64) -> i64 {
        sqlx::query_scalar("SELECT sync_config_cache_version FROM principals WHERE id = $1")
            .bind(principal_id)
            .fetch_one(&self.app.db.pool)
            .await
            .unwrap()
    }
}

/// A principal granted `secret_id` directly.
pub async fn grant(f: &Fixture, secret_id: i64) -> i64 {
    let principal = f.app.create_principal("agent", "grantee", f.admin.id).await;
    sqlx::query(
        "INSERT INTO grants (principal_id, static_secret_id, created_by_id, priority, created_at, updated_at) \
         VALUES ($1, $2, $3, 0, now(), now())",
    )
    .bind(principal)
    .bind(secret_id)
    .bind(f.admin.id)
    .execute(&f.app.db.pool)
    .await
    .unwrap();
    principal
}

pub fn assert_no(text: &str, secrets: &[&str]) {
    for secret in secrets {
        assert!(!text.contains(secret), "{secret} leaked in {text}");
    }
}

impl Fixture {
    pub async fn count(&self, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&self.app.db.pool)
            .await
            .unwrap()
    }

    /// A credential minted through the app with `slug`.
    pub async fn insert_credential_for(&self, slug: &str) -> i64 {
        let now = self.app.now().naive_utc();
        sqlx::query_scalar(
            "INSERT INTO broker_credentials (oauth_app_id, provider_subject, provider_email, \
             token_endpoint, created_by_id, created_at, updated_at) \
             VALUES ((SELECT id FROM oauth_apps WHERE slug = $1), 'subject-1', 'someone@example.test', \
             'https://idp.example/token', $2, $3, $3) RETURNING id",
        )
        .bind(slug)
        .bind(self.admin.id)
        .bind(now)
        .fetch_one(&self.app.db.pool)
        .await
        .unwrap()
    }
}
