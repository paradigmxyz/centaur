//! Test helpers for area tests (feature `testing`).
//!
//! ```ignore
//! let Some(app) = TestApp::new(my_area::routes()).await else { return };
//! let admin = app.create_user("admin@example.test", true, "active").await;
//! let key = app.create_api_key(admin.id).await;
//! assert_eq!(app.get("/api/v1/things").bearer(&key).send().await.status(), 200);
//!
//! let mut browser = app.client();
//! browser.sign_in(admin.id);
//! let created = browser.send(app.post("/api/v1/console/things").json(&body).csrf(&browser)).await;
//!
//! app.assert_console_access(Access::Admin, Method::GET, "/api/v1/console/things", None).await;
//! ```
//!
//! Each [`TestApp`] has its own database cloned from a migrated template
//! (`centaur_console_db::testing`; tests skip without
//! `CONSOLE_DB_TEST_DATABASE_URL`), a fixed clock at [`TEST_NOW`], and
//! requests default to `Host: www.example.com`.

use std::{collections::BTreeMap, path::PathBuf};

use chrono::{DateTime, Utc};

use axum::body::Body;
use bytes::Bytes;
use centaur_console_auth::{
    flow_cookie::FlowCookies,
    session::SessionConfig,
    state_token::StateSigner,
    tokens::{self, SandboxJwtConfig},
};
use centaur_console_db::{Clock, Encryptor, testing::TestDb};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use super::{
    app::router,
    assets::Assets,
    auth::Access,
    hosts::HostAllowlist,
    routes::Routes,
    state::{AppState, Config, Inner},
};
use crate::env::Env;

pub const TEST_SECRET_KEY_BASE: &str =
    "test-secret-key-base-0000000000000000000000000000000000000000";
pub const TEST_JWT_SECRET: &str = "test-jwt-signing-secret";
pub const TEST_HOST: &str = "www.example.com";
/// The `Origin` a same-origin browser request sends.
pub const TEST_ORIGIN: &str = "http://www.example.com";
/// The instant `seconds` after the Unix epoch.
pub fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).expect("valid time")
}

/// 2026-11-13T03:00:00Z.
pub const TEST_NOW: i64 = 1_794_538_800;
pub const TEST_PASSWORD: &str = "password123456";

/// `services/console` in this checkout.
pub fn console_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../services/console")
}

/// A console app on its own database.
pub struct TestApp {
    pub state: AppState,
    pub router: axum::Router,
    pub db: TestDb,
    pub clock: Clock,
    _spa: tempfile::TempDir,
}

/// A user the helpers created.
#[derive(Clone, Debug)]
pub struct TestUser {
    pub id: i64,
    pub email: String,
}

impl TestApp {
    /// The app serving `routes`. `None` (skip the test) without a database.
    pub async fn new(routes: Routes) -> Option<Self> {
        Self::with_env(routes, &[]).await
    }

    /// Like [`TestApp::new`] with environment settings (`(name, value)`;
    /// area settings are read from `state.config.env`).
    pub async fn with_env(routes: Routes, env: &[(&str, &str)]) -> Option<Self> {
        Self::build(routes, env, |_| {}).await
    }

    pub async fn build(
        routes: Routes,
        env: &[(&str, &str)],
        configure: impl FnOnce(&mut Config),
    ) -> Option<Self> {
        Self::build_full(routes, env, configure, false).await
    }

    /// Like [`TestApp::build`]; with `api_db_is_test_db` the api-rs database
    /// handle (workflow views) is the test database itself.
    pub async fn build_full(
        routes: Routes,
        env: &[(&str, &str)],
        configure: impl FnOnce(&mut Config),
        api_db_is_test_db: bool,
    ) -> Option<Self> {
        let db = TestDb::migrated().await?;
        let spa = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            spa.path().join("index.html"),
            "<!doctype html><div id=\"root\"></div>",
        )
        .expect("write the SPA shell");
        let public_dir = console_dir().join("public");
        let mut pairs = env.to_vec();
        if !pairs.iter().any(|(k, _)| k.ends_with("_ALLOWED_HOSTS")) {
            pairs.push(("CENTAUR_CONSOLE_ALLOWED_HOSTS", TEST_HOST));
        }
        let env = Env::from_pairs(pairs);
        let mut config = Config {
            public_url: env.present("PUBLIC_URL").map(str::to_owned),
            jwt_signing_secret: TEST_JWT_SECRET.into(),
            sandbox_jwt: SandboxJwtConfig::default(),
            public_dir: public_dir.clone(),
            spa_index: spa.path().join("index.html"),
            env,
        };
        configure(&mut config);
        let clock = Clock::fixed(at(TEST_NOW));
        let hosts = HostAllowlist::from_config(&config);
        let state = AppState::new(Inner {
            db: db.pool.clone(),
            api_db: api_db_is_test_db.then(|| db.pool.clone()),
            assets: Assets::new(&config.public_dir),
            http: super::http::client(&config.env).expect("HTTP client"),
            config,
            encryptor: Encryptor::development(),
            clock: clock.clone(),
            sessions: SessionConfig::new(TEST_SECRET_KEY_BASE),
            flow_cookies: FlowCookies::new(TEST_SECRET_KEY_BASE),
            signer: StateSigner::new(TEST_SECRET_KEY_BASE),
            hosts,
        });
        Some(Self {
            router: router(state.clone(), routes),
            state,
            db,
            clock,
            _spa: spa,
        })
    }

    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.state.now()
    }

    pub fn request(&self, method: Method, path: &str) -> TestRequest {
        TestRequest::new(self.router.clone(), method, path)
    }

    pub fn get(&self, path: &str) -> TestRequest {
        self.request(Method::GET, path)
    }

    pub fn post(&self, path: &str) -> TestRequest {
        self.request(Method::POST, path)
    }

    pub fn put(&self, path: &str) -> TestRequest {
        self.request(Method::PUT, path)
    }

    pub fn patch(&self, path: &str) -> TestRequest {
        self.request(Method::PATCH, path)
    }

    pub fn delete(&self, path: &str) -> TestRequest {
        self.request(Method::DELETE, path)
    }

    /// A browser: keeps cookies between requests.
    pub fn client(&self) -> TestClient {
        TestClient {
            state: self.state.clone(),
            cookies: BTreeMap::new(),
            csrf: None,
        }
    }

    // --- fixtures ----------------------------------------------------------

    /// A user with [`TEST_PASSWORD`] (bcrypt cost 4).
    pub async fn create_user(&self, email: &str, admin: bool, status: &str) -> TestUser {
        let digest = centaur_console_auth::password::hash(TEST_PASSWORD, 4).await;
        let now = self.now().naive_utc();
        let id = sqlx::query_scalar(
            "INSERT INTO users (email, admin, status, password_digest, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $5) RETURNING id",
        )
        .bind(email)
        .bind(admin)
        .bind(status)
        .bind(digest)
        .bind(now)
        .fetch_one(&self.db.pool)
        .await
        .expect("insert user");
        TestUser {
            id,
            email: email.to_owned(),
        }
    }

    /// A bearer API key for `user_id`; returns the plaintext token.
    pub async fn create_api_key(&self, user_id: i64) -> String {
        let token = format!("iak_{}", hex::encode(rand_bytes()));
        let now = self.now().naive_utc();
        sqlx::query(
            "INSERT INTO api_keys (user_id, name, token_hash, created_at, updated_at) \
             VALUES ($1, 'test key', $2, $3, $3)",
        )
        .bind(user_id)
        .bind(centaur_console_auth::bearer::hash_token(&token))
        .bind(now)
        .execute(&self.db.pool)
        .await
        .expect("insert API key");
        token
    }

    /// A principal of `kind` created by `created_by`.
    pub async fn create_principal(&self, kind: &str, name: &str, created_by: i64) -> i64 {
        let now = self.now().naive_utc();
        sqlx::query_scalar(
            "INSERT INTO principals (kind, name, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $4) RETURNING id",
        )
        .bind(kind)
        .bind(name)
        .bind(created_by)
        .bind(now)
        .fetch_one(&self.db.pool)
        .await
        .expect("insert principal")
    }

    /// A sandbox proxy assigned to `principal_id`.
    pub async fn create_proxy(&self, name: &str, principal_id: i64) -> i64 {
        let now = self.now().naive_utc();
        sqlx::query_scalar(
            "INSERT INTO proxies (name, bearer_token_hash, principal_id, principal_assigned_at, \
             created_at, updated_at) VALUES ($1, 'unused', $2, $3, $3, $3) RETURNING id",
        )
        .bind(name)
        .bind(principal_id)
        .bind(now)
        .fetch_one(&self.db.pool)
        .await
        .expect("insert proxy")
    }

    /// The entitlement token a sandbox presents for `proxy_id`/`principal_id`.
    pub async fn sandbox_token(&self, proxy_id: i64, principal_id: i64) -> String {
        let name: String = sqlx::query_scalar("SELECT name FROM proxies WHERE id = $1")
            .bind(proxy_id)
            .fetch_one(&self.db.pool)
            .await
            .expect("proxy");
        tokens::sandbox_entitlement(
            TEST_JWT_SECRET,
            &self.state.config.sandbox_jwt,
            &centaur_console_db::oid("prx", proxy_id),
            &name,
            &centaur_console_db::oid("prn", principal_id),
            self.now(),
        )
        .expect("sandbox token")
    }

    /// The api-rs service token for `/internal/*`.
    pub fn internal_token(&self) -> String {
        tokens::internal_service(TEST_JWT_SECRET, self.now()).expect("service token")
    }

    // --- authorization matrices ----------------------------------------------

    /// Asserts who may call a console route. `body` is a valid JSON body for
    /// writes (the checks run before the handler, so any object works).
    ///
    /// anonymous 401 `session_required`; an `Authorization` header 403;
    /// disabled 401 `account_disabled`; pending 403 `account_pending` (unless
    /// `Access::Pending`); a non-admin 403 `admin_required` (unless
    /// `Access::Active`/`Pending`); writes without the CSRF token or from a
    /// foreign origin 403 `csrf`; everyone allowed gets past the guard.
    pub async fn assert_console_access(
        &self,
        access: Access,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) {
        let write = !matches!(method, Method::GET | Method::HEAD);
        let send = |client: &TestClient, csrf: bool| {
            let mut request = self.request(method.clone(), path);
            if let Some(body) = body {
                request = request.json(body);
            }
            if csrf {
                request = request.csrf(client);
            }
            request
        };
        let anonymous = self.client();
        let denied = |response: &TestResponse, status: u16, reason: &str, who: &str| {
            assert_eq!(
                (response.status(), response.error_reason().as_deref()),
                (status, Some(reason)),
                "{who} on {method} {path}: {}",
                response.text()
            );
        };
        denied(
            &send(&anonymous, false).send().await,
            401,
            "session_required",
            "anonymous",
        );

        let n = hex::encode(&rand_bytes()[..6]);
        let admin = self
            .create_user(&format!("matrix-admin-{n}@example.test"), true, "active")
            .await;
        let mut browser = self.client();
        browser.sign_in(admin.id);
        let response = browser
            .send(send(&browser, true).bearer("iak_anything"))
            .await;
        denied(&response, 403, "session_required", "a bearer key");

        let disabled = self
            .create_user(
                &format!("matrix-disabled-{n}@example.test"),
                false,
                "disabled",
            )
            .await;
        let mut browser = self.client();
        browser.sign_in(disabled.id);
        denied(
            &browser.send(send(&browser, true)).await,
            401,
            "account_disabled",
            "disabled",
        );

        let pending = self
            .create_user(
                &format!("matrix-pending-{n}@example.test"),
                false,
                "pending",
            )
            .await;
        let mut browser = self.client();
        browser.sign_in(pending.id);
        let response = browser.send(send(&browser, true)).await;
        if access == Access::Pending {
            assert_passes(&response, "pending", &method, path);
        } else {
            denied(&response, 403, "account_pending", "pending");
        }

        let member = self
            .create_user(&format!("matrix-member-{n}@example.test"), false, "active")
            .await;
        let mut browser = self.client();
        browser.sign_in(member.id);
        let response = browser.send(send(&browser, true)).await;
        if access == Access::Admin {
            denied(&response, 403, "admin_required", "a non-admin");
        } else {
            assert_passes(&response, "an active member", &method, path);
        }

        // A fresh session per check: the call itself may sign the caller out.
        let fresh_admin = || {
            let mut browser = self.client();
            browser.sign_in(admin.id);
            browser
        };
        let mut browser = fresh_admin();
        assert_passes(
            &browser.send(send(&browser, true)).await,
            "an admin",
            &method,
            path,
        );
        if write {
            let mut browser = fresh_admin();
            denied(
                &browser.send(send(&browser, false)).await,
                403,
                "csrf",
                "no CSRF token",
            );
            // A browser always sends Origin on a write; without it the
            // request's origin cannot be checked, so even a valid token is
            // not enough.
            let mut browser = fresh_admin();
            let response = browser.send(send(&browser, true).without_origin()).await;
            denied(&response, 403, "csrf", "no Origin");
            let mut browser = fresh_admin();
            let response = browser
                .send(send(&browser, true).header("origin", "https://evil.example"))
                .await;
            denied(&response, 403, "csrf", "a foreign origin");
            let mut browser = fresh_admin();
            let response = browser
                .send(send(&browser, false).header("x-csrf-token", "not-the-token"))
                .await;
            denied(&response, 403, "csrf", "a wrong CSRF token");
            // A forwarded host is only trusted when it is one we answer to,
            // so a spoofed one cannot make a foreign Origin look local.
            let mut browser = fresh_admin();
            let response = browser
                .send(
                    send(&browser, true)
                        .header("origin", "https://evil.example")
                        .header("x-forwarded-host", "evil.example"),
                )
                .await;
            denied(&response, 403, "csrf", "a spoofed forwarded host");
        }
    }

    /// Asserts who may call a bearer route: no key, an unknown key, a
    /// disabled owner, and a non-admin owner are refused; an admin's key
    /// gets past the guard.
    pub async fn assert_bearer_access(&self, method: Method, path: &str, body: Option<&Value>) {
        let send = |token: Option<&str>| {
            let mut request = self.request(method.clone(), path);
            if let Some(body) = body {
                request = request.json(body);
            }
            match token {
                Some(token) => request.bearer(token),
                None => request,
            }
        };
        let status = async |token: Option<&str>| send(token).send().await.status();
        assert_eq!(status(None).await, 401, "no key on {method} {path}");
        assert_eq!(
            status(Some("iak_unknown")).await,
            401,
            "unknown key on {method} {path}"
        );
        let n = hex::encode(&rand_bytes()[..6]);
        for (email, admin, user_status, expected) in [
            (
                format!("bearer-disabled-{n}@example.test"),
                true,
                "disabled",
                401,
            ),
            (
                format!("bearer-pending-{n}@example.test"),
                true,
                "pending",
                401,
            ),
            (
                format!("bearer-member-{n}@example.test"),
                false,
                "active",
                403,
            ),
        ] {
            let user = self.create_user(&email, admin, user_status).await;
            let key = self.create_api_key(user.id).await;
            assert_eq!(
                status(Some(&key)).await,
                expected,
                "{email} on {method} {path}"
            );
        }
        let admin = self
            .create_user(&format!("bearer-admin-{n}@example.test"), true, "active")
            .await;
        let key = self.create_api_key(admin.id).await;
        let response = send(Some(&key)).send().await;
        assert_passes(&response, "an admin key", &method, path);
        // A deleted key stops working.
        sqlx::query("UPDATE api_keys SET deleted_at = now() WHERE user_id = $1")
            .bind(admin.id)
            .execute(&self.db.pool)
            .await
            .expect("delete key");
        assert_eq!(
            status(Some(&key)).await,
            401,
            "deleted key on {method} {path}"
        );
    }
}

/// Whether `response` is a credential guard's refusal (a handler may still
/// answer 403 for its own reasons).
fn guard_refusal(response: &TestResponse) -> bool {
    const REASONS: [&str; 6] = [
        "session_required",
        "account_disabled",
        "account_pending",
        "csrf",
        "admin_required",
        "credential_namespace_mismatch",
    ];
    const MESSAGES: [&str; 2] = [
        "invalid or missing API key",
        "API key owner is not an admin",
    ];
    matches!(response.status(), 401 | 403)
        && (response
            .error_reason()
            .is_some_and(|reason| REASONS.contains(&reason.as_str()))
            || response
                .error_message()
                .is_some_and(|message| MESSAGES.contains(&message.as_str())))
}

fn assert_passes(response: &TestResponse, who: &str, method: &Method, path: &str) {
    assert!(
        !guard_refusal(response),
        "{who} was refused on {method} {path}: {} {}",
        response.status(),
        response.text()
    );
}

fn rand_bytes() -> [u8; 32] {
    centaur_console_auth::random::random_bytes::<32>()
}

// --- requests and responses ----------------------------------------------------

pub struct TestRequest {
    app: axum::Router,
    method: Method,
    path: String,
    headers: Vec<(HeaderName, HeaderValue)>,
    /// The `Origin` a browser sends, set by [`TestRequest::csrf`] unless a
    /// test sets its own or asks for none.
    origin: Option<String>,
    body: Bytes,
}

impl TestRequest {
    fn new(app: axum::Router, method: Method, path: &str) -> Self {
        Self {
            app,
            method,
            path: path.to_owned(),
            headers: vec![(header::HOST, HeaderValue::from_static(TEST_HOST))],
            origin: None,
            body: Bytes::new(),
        }
    }

    /// No `Origin` header at all, as a non-browser client sends.
    pub fn without_origin(mut self) -> Self {
        self.origin = None;
        self.headers.retain(|(name, _)| *name != header::ORIGIN);
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        let name = HeaderName::from_bytes(name.as_bytes()).expect("header name");
        self.headers.retain(|(existing, _)| *existing != name);
        self.headers
            .push((name, HeaderValue::from_str(value).expect("header value")));
        self
    }

    pub fn bearer(self, token: &str) -> Self {
        self.header("authorization", &format!("Bearer {token}"))
    }

    pub fn accept_html(self) -> Self {
        self.header("accept", "text/html,application/xhtml+xml")
    }

    pub fn json(self, value: &Value) -> Self {
        let mut request = self.header("content-type", "application/json");
        request.body = Bytes::from(value.to_string());
        request
    }

    pub fn form(self, pairs: &[(&str, &str)]) -> Self {
        let mut request = self.header("content-type", "application/x-www-form-urlencoded");
        request.body = Bytes::from(serde_urlencoded::to_string(pairs).expect("form"));
        request
    }

    pub fn raw_body(self, content_type: &str, body: &[u8]) -> Self {
        let mut request = self.header("content-type", content_type);
        request.body = Bytes::copy_from_slice(body);
        request
    }

    /// Sends the client's CSRF token and a same-origin `Origin`, as the SPA
    /// does on every write. A write is refused without both.
    pub fn csrf(mut self, client: &TestClient) -> Self {
        if let Some(token) = &client.csrf {
            self = self.header("x-csrf-token", token);
        }
        self.origin = Some(TEST_ORIGIN.to_owned());
        self
    }

    pub async fn send(self) -> TestResponse {
        let mut builder = Request::builder().method(self.method).uri(self.path);
        let mut headers = self.headers;
        if let Some(origin) = &self.origin
            && !headers.iter().any(|(name, _)| *name == header::ORIGIN)
        {
            headers.push((
                header::ORIGIN,
                HeaderValue::from_str(origin).expect("origin value"),
            ));
        }
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let response = self
            .app
            .oneshot(builder.body(Body::from(self.body)).expect("request"))
            .await
            .expect("infallible");
        let (parts, body) = response.into_parts();
        TestResponse {
            status: parts.status,
            headers: parts.headers,
            body: body.collect().await.expect("body").to_bytes(),
        }
    }
}

#[derive(Debug)]
pub struct TestResponse {
    status: StatusCode,
    pub headers: HeaderMap,
    body: Bytes,
}

impl TestResponse {
    pub fn status(&self) -> u16 {
        self.status.as_u16()
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    pub fn location(&self) -> Option<&str> {
        self.header("location")
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("not JSON ({error}): {}", self.text()))
    }

    pub fn error_message(&self) -> Option<String> {
        serde_json::from_slice::<Value>(&self.body).ok()?["error"]["message"]
            .as_str()
            .map(str::to_owned)
    }

    pub fn error_reason(&self) -> Option<String> {
        serde_json::from_slice::<Value>(&self.body)
            .ok()?
            .pointer("/error/details/reason")?
            .as_str()
            .map(str::to_owned)
    }

    pub fn set_cookies(&self) -> Vec<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_owned))
            .collect()
    }
}

/// A browser session: cookies persist across requests.
pub struct TestClient {
    state: AppState,
    cookies: BTreeMap<String, String>,
    csrf: Option<String>,
}

impl TestClient {
    /// Starts a session already signed in as `user_id` (as after login).
    pub fn sign_in(&mut self, user_id: i64) {
        let now = self.state.now();
        let mut session = self.state.sessions.load(None, now);
        session.sign_in(user_id, now);
        self.csrf = Some(session.csrf_token().to_owned());
        let cookie = self
            .state
            .sessions
            .set_cookie(&session, false, now)
            .expect("session cookie");
        self.remember(&cookie);
    }

    /// Starts an anonymous session (as any page that shows a form does) and
    /// returns its CSRF token.
    pub fn start_anonymous_session(&mut self) -> String {
        let now = self.state.now();
        let mut session = self.state.sessions.load(None, now);
        let token = session.csrf_token().to_owned();
        let cookie = self
            .state
            .sessions
            .set_cookie(&session, false, now)
            .expect("session cookie");
        self.remember(&cookie);
        self.csrf = Some(token.clone());
        token
    }

    pub fn csrf_token(&self) -> Option<&str> {
        self.csrf.as_deref()
    }

    pub fn has_cookie(&self, name: &str) -> bool {
        self.cookies.contains_key(name)
    }

    fn remember(&mut self, set_cookie: &str) {
        let Some((name, rest)) = set_cookie
            .split(';')
            .next()
            .and_then(|pair| pair.split_once('='))
        else {
            return;
        };
        let expired = set_cookie.contains("Max-Age=0");
        if expired || rest.is_empty() {
            self.cookies.remove(name);
        } else {
            self.cookies.insert(name.to_owned(), rest.to_owned());
        }
    }

    /// Sends `request` with this browser's cookies and keeps what it sets.
    pub async fn send(&mut self, request: TestRequest) -> TestResponse {
        let mut request = request;
        if !self.cookies.is_empty() {
            let cookie = self
                .cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join("; ");
            request = request.header("cookie", &cookie);
        }
        let response = request.send().await;
        for cookie in response.set_cookies() {
            self.remember(&cookie);
        }
        response
    }
}
