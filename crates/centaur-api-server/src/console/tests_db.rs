//! Database-backed tests for the Console foundation.
//!
//! Every test needs `CONSOLE_DB_TEST_DATABASE_URL` pointing at a database
//! prepared with Console's schema. Tests insert uniquely named rows and share
//! one database, so they can run in parallel. Without the variable they skip
//! locally and fail in CI, matching the rest of the Console database tests.

use std::env;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
    response::Response,
};
use bcrypt::{DEFAULT_COST, hash};
use centaur_console_db::oid;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;

use crate::{
    ApiAuthConfig, AppState, build_router_with_app_state,
    console::{
        bearer_router,
        guard::GuardedRoutes,
        sandbox::{SandboxJwtConfig, test_sandbox_routes},
    },
    console_session::ConsoleSessionConfig,
};

const SESSION_SECRET: &str =
    "test-only-console-session-secret-0000000000000000000000000000000000000000000000000000";
const CONSOLE_HOST: &str = "console.example.test";
const JWT_SECRET: &str = "test-only-sandbox-jwt-secret";
const PASSWORD: &str = "fake-test-password";

/// The shared Console database pool, or `None` when the test should skip.
/// `areas/oauth/tests_db.rs` and the other Console `tests_db` modules use it.
pub(crate) async fn test_pool() -> Option<PgPool> {
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

fn unique() -> String {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).expect("operating system randomness");
    hex::encode(bytes)
}

struct Fixture {
    pool: PgPool,
    unique: String,
    admin: i64,
    admin_email: String,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let pool = test_pool().await?;
        let unique = unique();
        let admin_email = format!("admin-{unique}@example.com");
        let admin = insert_user(
            &pool,
            &admin_email,
            "active",
            true,
            Some(hash(PASSWORD, DEFAULT_COST).unwrap().as_str()),
        )
        .await;
        Some(Self {
            pool,
            unique,
            admin,
            admin_email,
        })
    }

    fn name(&self, label: &str) -> String {
        format!("{label}-{}", self.unique)
    }

    async fn user(&self, label: &str, status: &str, admin: bool) -> (i64, String) {
        let email = format!("{}-{}@example.com", self.name(label), self.unique);
        let id = insert_user(&self.pool, &email, status, admin, None).await;
        (id, email)
    }

    async fn api_key(&self, user_id: i64, token: &str, deleted: bool) -> String {
        let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
        sqlx::query(
            "INSERT INTO api_keys (name, token_hash, user_id, deleted_at, created_at, updated_at) \
             VALUES ($1, $2, $3, CASE WHEN $4 THEN now() ELSE NULL END, now(), now())",
        )
        .bind(self.name("key"))
        .bind(&token_hash)
        .bind(user_id)
        .bind(deleted)
        .execute(&self.pool)
        .await
        .unwrap();
        token_hash
    }

    async fn principal(&self, kind: &str, team_id: Option<&str>, console_user_id: i64) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO principals \
                (foreign_id, name, kind, labels, slack_user_id, slack_team_id, console_user_id, \
                 created_by_id, created_at, updated_at) \
             VALUES ($1, 'Example', $2, '{}', 'U12345678', $3, $4, $5, now(), now()) \
             RETURNING id",
        )
        .bind(format!("{}-{kind}", self.name("principal")))
        .bind(kind)
        .bind(team_id)
        .bind(console_user_id)
        .bind(self.admin)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    async fn proxy(&self, principal_id: i64) -> (i64, String) {
        let name = self.name("sandbox");
        let id = sqlx::query_scalar(
            "INSERT INTO proxies \
                (name, bearer_token_hash, principal_id, principal_assigned_at, created_at, updated_at) \
             VALUES ($1, $2, $3, now(), now(), now()) RETURNING id",
        )
        .bind(&name)
        .bind(hex::encode(Sha256::digest(name.as_bytes())))
        .bind(principal_id)
        .fetch_one(&self.pool)
        .await
        .unwrap();
        (id, name)
    }
}

async fn insert_user(
    pool: &PgPool,
    email: &str,
    status: &str,
    admin: bool,
    password_digest: Option<&str>,
) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO users (email, admin, status, password_digest, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, now(), now()) RETURNING id",
    )
    .bind(email)
    .bind(admin)
    .bind(status)
    .bind(password_digest)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn app(fixture: &Fixture) -> Router {
    let state = AppState::unready(ApiAuthConfig::testing("test-secret"))
        .with_console_db(fixture.pool.clone())
        .with_console_write_db(fixture.pool.clone())
        .with_console_session(
            ConsoleSessionConfig::from_settings(
                Some(SESSION_SECRET),
                Some("https://console.example.test"),
            )
            .unwrap()
            .unwrap(),
        );
    build_router_with_app_state(state)
}

fn request(method: Method, uri: &str, headers: &[(&str, &str)], body: Body) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"))
    {
        request = request.header(header::HOST, CONSOLE_HOST);
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.body(body).unwrap()
}

async fn send(app: &Router, request: Request<Body>) -> Response {
    app.clone().oneshot(request).await.unwrap()
}

async fn send_console_roles(app: &Router, auth: &str) -> Response {
    send(
        app,
        request(
            Method::GET,
            "/api/v1/console/roles",
            &[("authorization", auth)],
            Body::empty(),
        ),
    )
    .await
}

fn cookie(response: &Response) -> Option<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("_centaur_console_session="))
        .map(|value| value.split(';').next().unwrap().to_owned())
}

fn origin() -> (&'static str, &'static str) {
    ("origin", "https://console.example.test")
}

async fn json_body(response: Response) -> Value {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(!bytes.is_empty(), "expected a body, status was {status}");
    serde_json::from_slice(&bytes).unwrap()
}

async fn login(app: &Router, email: &str, password: &str) -> Response {
    let response = send(app, request(Method::GET, "/login", &[], Body::empty())).await;
    assert_eq!(response.status(), StatusCode::OK);
    let login_cookie = cookie(&response).expect("login sets a session cookie");
    send(
        app,
        request(
            Method::POST,
            "/login",
            &[
                origin(),
                ("cookie", login_cookie.as_str()),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            form(&[("email", email), ("password", password)]),
        ),
    )
    .await
}

fn form(fields: &[(&str, &str)]) -> Body {
    Body::from(
        fields
            .iter()
            .map(|(name, value)| {
                format!(
                    "{}={}",
                    urlencoding::encode(name),
                    urlencoding::encode(value)
                )
            })
            .collect::<Vec<_>>()
            .join("&"),
    )
}

#[tokio::test]
async fn console_session_db_login_me_origin_write_and_logout() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let app = app(&fixture);

    let response = send(&app, request(Method::GET, "/login", &[], Body::empty())).await;
    assert_eq!(response.status(), StatusCode::OK);
    let login_cookie = cookie(&response).expect("login sets a session cookie");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let html = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(
        !html.contains("authenticity_token"),
        "forms no longer carry a CSRF token"
    );

    // Logout keeps the login gate: a signed-out browser is sent back to the
    // form instead of getting an origin rejection. Logout is `POST /logout`;
    // there is no `_method` override.
    let response = send(&app, request(Method::POST, "/logout", &[], Body::empty())).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/login");

    // HTML forms answer a missing or cross-site Origin with a 422 page.
    let response = send(
        &app,
        request(
            Method::POST,
            "/login",
            &[
                ("cookie", login_cookie.as_str()),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            form(&[
                ("email", fixture.admin_email.as_str()),
                ("password", PASSWORD),
            ]),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let mut cross_origin = request(
        Method::POST,
        "/login",
        &[
            ("cookie", login_cookie.as_str()),
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        form(&[
            ("email", fixture.admin_email.as_str()),
            ("password", PASSWORD),
        ]),
    );
    cross_origin
        .headers_mut()
        .insert(header::ORIGIN, "https://evil.example.test".parse().unwrap());
    let response = send(&app, cross_origin).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let response = send(
        &app,
        request(
            Method::POST,
            "/login",
            &[
                origin(),
                ("cookie", login_cookie.as_str()),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            form(&[
                ("email", fixture.admin_email.as_str()),
                ("password", PASSWORD),
            ]),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "/console/principals"
    );
    let signed_in_cookie = cookie(&response).expect("sign-in rotates the session cookie");
    assert_ne!(signed_in_cookie, login_cookie);

    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/me",
            &[("cookie", signed_in_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let me_cookie = cookie(&response).unwrap_or_else(|| signed_in_cookie.clone());
    let me = json_body(response).await;
    assert_eq!(me["data"]["user"]["email"], fixture.admin_email);
    assert_eq!(me["data"]["user"]["admin"], true);
    assert_eq!(me["data"]["auth_method"], "session");
    assert!(
        me["data"].get("flash").is_none(),
        "the session API has no flash field"
    );
    assert!(
        me["data"].get("csrf_token").is_none(),
        "/me no longer exposes a CSRF token"
    );

    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/me",
            &[("cookie", me_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    let second_me = json_body(response).await;
    assert!(second_me["data"].get("flash").is_none());

    let response = send(
        &app,
        request(
            Method::POST,
            "/api/v1/console/probe/admin-write",
            &[origin(), ("cookie", me_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = send(
        &app,
        request(
            Method::POST,
            "/api/v1/console/probe/admin-write",
            &[("cookie", me_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(response).await["error"]["details"]["reason"],
        "csrf"
    );

    let response = send(
        &app,
        request(
            Method::POST,
            "/api/v1/console/probe/admin-write",
            &[
                ("cookie", me_cookie.as_str()),
                ("origin", "https://evil.example.test"),
            ],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = send(
        &app,
        request(
            Method::DELETE,
            "/api/v1/console/session",
            &[origin(), ("cookie", me_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let cleared = cookie(&response).expect("logout resets the session cookie");

    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/me",
            &[("cookie", cleared.as_str())],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn console_session_db_disabled_and_pending_accounts_are_denied_with_console_bodies() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let app = app(&fixture);
    let password_digest = hash(PASSWORD, DEFAULT_COST).unwrap();
    let disabled_email = format!("disabled-{}@example.com", fixture.unique);
    insert_user(
        &fixture.pool,
        &disabled_email,
        "disabled",
        false,
        Some(&password_digest),
    )
    .await;
    let pending_email = format!("pending-{}@example.com", fixture.unique);
    insert_user(
        &fixture.pool,
        &pending_email,
        "pending",
        false,
        Some(&password_digest),
    )
    .await;

    let response = login(&app, &disabled_email, PASSWORD).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let html = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains("Your account is disabled."));

    let response = login(&app, &pending_email, PASSWORD).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "/pending"
    );
    let pending_cookie = cookie(&response).unwrap();

    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/me",
            &[("cookie", pending_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let me = json_body(response).await;
    assert_eq!(me["data"]["user"]["status"], "pending");
    assert_eq!(me["data"]["user"]["admin"], false);

    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/probe/admin",
            &[("cookie", pending_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(response).await["error"]["details"]["reason"],
        "account_pending"
    );

    let response = send(
        &app,
        request(
            Method::GET,
            "/pending",
            &[("cookie", pending_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(html.contains(&pending_email));
    assert!(html.contains("is pending approval by an administrator."));
    let response = send(
        &app,
        request(
            Method::POST,
            "/logout",
            &[
                origin(),
                ("cookie", pending_cookie.as_str()),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/login");
    assert!(
        cookie(&response).is_some(),
        "logout clears the session cookie"
    );

    // A valid cookie is re-checked against the database on every request. Move
    // the signed-in admin to disabled and the guard must clear the session.
    let response = login(&app, &fixture.admin_email, PASSWORD).await;
    let admin_cookie = cookie(&response).unwrap();
    sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
        .bind(fixture.admin)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/me",
            &[("cookie", admin_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let cleared_cookie = cookie(&response).expect("a disabled account clears the session cookie");
    assert_eq!(
        json_body(response).await["error"]["details"]["reason"],
        "account_disabled"
    );
    let response = send(
        &app,
        request(
            Method::GET,
            "/login",
            &[("cookie", cleared_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(!html.contains("role=\"alert\""));
    assert!(!html.contains("Your account is disabled."));
    sqlx::query("UPDATE users SET status = 'active' WHERE id = $1")
        .bind(fixture.admin)
        .execute(&fixture.pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn console_session_db_bearer_key_requires_active_admin_owner() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let state = AppState::unready(ApiAuthConfig::testing("test-secret"))
        .with_console_db(fixture.pool.clone())
        .with_console_write_db(fixture.pool.clone());
    let app = Router::new()
        .nest("/api/v1", bearer_router(state.clone()))
        .with_state(state);

    let token = format!("fake-api-key-{}", fixture.unique);
    fixture.api_key(fixture.admin, &token, false).await;
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/probe/bearer/admin",
            &[("authorization", &format!("Bearer {token}"))],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["user_id"], fixture.admin);

    let (non_admin, _) = fixture.user("non-admin", "active", false).await;
    let non_admin_token = format!("fake-api-key-non-admin-{}", fixture.unique);
    fixture.api_key(non_admin, &non_admin_token, false).await;
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/probe/bearer/admin",
            &[("authorization", &format!("Bearer {non_admin_token}"))],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(response).await["error"]["message"],
        "API key owner is not an admin"
    );

    let (inactive, _) = fixture.user("inactive", "disabled", true).await;
    let inactive_token = format!("fake-api-key-inactive-{}", fixture.unique);
    fixture.api_key(inactive, &inactive_token, false).await;
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/probe/bearer/admin",
            &[("authorization", &format!("Bearer {inactive_token}"))],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(response).await["error"]["message"],
        "invalid or missing API key"
    );

    let deleted_token = format!("fake-api-key-deleted-{}", fixture.unique);
    fixture.api_key(fixture.admin, &deleted_token, true).await;
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/probe/bearer/admin",
            &[("authorization", &format!("Bearer {deleted_token}"))],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn console_session_db_bearer_api_keys_list_show_revoke() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let state = AppState::unready(ApiAuthConfig::testing("test-secret"))
        .with_console_db(fixture.pool.clone())
        .with_console_write_db(fixture.pool.clone());
    let app = Router::new()
        .nest("/api/v1", bearer_router(state.clone()))
        .with_state(state);

    let auth_token = format!("fake-api-key-owner-{}", fixture.unique);
    let auth_hash = fixture.api_key(fixture.admin, &auth_token, false).await;
    let auth = ("authorization", format!("Bearer {auth_token}"));
    let auth_header = [(auth.0, auth.1.as_str())];

    // Create: the response carries the one-time token and the Console payload.
    let response = send(
        &app,
        request(
            Method::POST,
            "/api/v1/api_keys",
            &[auth_header[0], ("content-type", "application/json")],
            Body::from(json!({"data": {"name": fixture.name("created")}}).to_string()),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = json_body(response).await;
    let created_id = created["data"]["id"].as_str().unwrap().to_owned();
    assert!(created_id.starts_with("ak_"));
    assert!(
        created["data"]["token"]
            .as_str()
            .unwrap()
            .starts_with("iak_")
    );
    assert_eq!(created["data"]["name"], fixture.name("created"));

    // A blank name is the shared FieldErrors 422 envelope.
    let response = send(
        &app,
        request(
            Method::POST,
            "/api/v1/api_keys",
            &[auth_header[0], ("content-type", "application/json")],
            Body::from(json!({"data": {"name": "  "}}).to_string()),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(response).await;
    assert_eq!(body["error"]["message"], "validation failed");
    assert!(
        body["error"]["details"]["name"]
            .as_array()
            .is_some_and(|messages| !messages.is_empty()),
        "{body}"
    );

    // The list goes through the shared count/page queries and page meta.
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/api_keys?limit=1&page=1",
            &auth_header,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json_body(response).await;
    assert_eq!(body["data"].as_array().unwrap().len(), 1);
    assert_eq!(body["meta"]["limit"], 1);
    assert_eq!(body["meta"]["page"], 1);
    assert!(body["meta"]["total"].as_i64().unwrap() >= 2);
    assert!(body["meta"]["total_pages"].as_i64().unwrap() >= 2);

    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/api_keys?page=2&limit=1",
            &auth_header,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        json_body(response).await["data"].as_array().unwrap().len(),
        1
    );

    // Show by oid, and another owner's key stays not found.
    let response = send(
        &app,
        request(
            Method::GET,
            &format!("/api/v1/api_keys/{created_id}"),
            &auth_header,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let shown = json_body(response).await;
    for field in ["id", "name", "created_at", "updated_at"] {
        assert_eq!(shown["data"][field], created["data"][field], "{field}");
    }
    assert!(shown["data"]["token"].is_null());

    let (other, _) = fixture.user("other-owner", "active", true).await;
    let other_hash = fixture
        .api_key(other, &format!("other-{}", fixture.unique), false)
        .await;
    let other_id: i64 = sqlx::query_scalar("SELECT id FROM api_keys WHERE token_hash = $1")
        .bind(&other_hash)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let response = send(
        &app,
        request(
            Method::GET,
            &format!("/api/v1/api_keys/{}", oid("ak", other_id)),
            &auth_header,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json_body(response).await["error"]["message"],
        "ApiKey not found"
    );

    // The key this request authenticated with cannot revoke itself.
    let auth_id: i64 = sqlx::query_scalar("SELECT id FROM api_keys WHERE token_hash = $1")
        .bind(&auth_hash)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    let response = send(
        &app,
        request(
            Method::DELETE,
            &format!("/api/v1/api_keys/{}", oid("ak", auth_id)),
            &auth_header,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Revoking soft-deletes: the payload's updated_at keeps its value.
    let response = send(
        &app,
        request(
            Method::DELETE,
            &format!("/api/v1/api_keys/{created_id}"),
            &auth_header,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let created_row: (i64, Option<time::PrimitiveDateTime>, bool) = sqlx::query_as(
        "SELECT id, deleted_at, updated_at = created_at FROM api_keys WHERE token_hash = $1",
    )
    .bind(
        created["data"]["token"]
            .as_str()
            .map(|token| hex::encode(Sha256::digest(token.as_bytes()))),
    )
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert!(created_row.1.is_some());
    assert!(created_row.2, "a soft delete leaves updated_at alone");

    let response = send(
        &app,
        request(
            Method::GET,
            &format!("/api/v1/api_keys/{created_id}"),
            &auth_header,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

fn sandbox_token(secret: &str, claims: &Value) -> String {
    encode(
        &Header::new(Algorithm::HS256),
        claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .unwrap()
}

fn sandbox_claims(proxy_id: &str, sandbox_id: &str, principal_id: &str, now: i64) -> Value {
    json!({
        "iss": crate::auth::SANDBOX_TOKEN_ISSUER,
        "aud": crate::auth::SANDBOX_TOKEN_AUDIENCE,
        "sub": principal_id,
        "iat": now,
        "exp": now + 3600,
        "sandbox_id": sandbox_id,
        "proxy_id": proxy_id,
        "capabilities": [
            "sessions.read",
            "workflows.read",
            "workflows.write",
            "sandbox.permissions",
            "sandbox.skills",
            "sandbox.tasks",
            "sandbox.oauth_apps",
            "console.delegate",
        ],
        "slack": {"upload_channels": [], "download_channels": [], "history_channels": []},
        "console_delegate": true,
    })
}

#[tokio::test]
async fn console_session_db_admin_manages_own_api_keys_from_a_session() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let app = app(&fixture);
    let response = login(&app, &fixture.admin_email, PASSWORD).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let session = cookie(&response).expect("signed-in session");
    let session_header = ("cookie", session.as_str());

    let response = send(
        &app,
        request(
            Method::POST,
            "/api/v1/console/api_keys",
            &[
                origin(),
                session_header,
                ("content-type", "application/json"),
            ],
            Body::from(r#"{"data":{"name":"laptop"}}"#),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let created = json_body(response).await;
    let token = created["data"]["token"].as_str().unwrap().to_owned();
    let id = created["data"]["id"].as_str().unwrap().to_owned();
    assert!(token.starts_with("iak_"));

    // The new key authenticates the bearer API as its owner.
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/api_keys",
            &[("authorization", &format!("Bearer {token}"))],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/api_keys",
            &[session_header],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let listed = json_body(response).await;
    let keys = listed["data"].as_array().unwrap();
    assert!(keys.iter().any(|key| key["id"] == id.as_str()));
    assert!(keys.iter().all(|key| key.get("token").is_none()));

    let response = send(
        &app,
        request(
            Method::DELETE,
            &format!("/api/v1/console/api_keys/{id}"),
            &[origin(), session_header],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/api_keys",
            &[("authorization", &format!("Bearer {token}"))],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn console_session_db_sandbox_token_checks_proxy_binding_and_admin() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let principal = fixture
        .principal("slack_dm", Some("T-console-home"), fixture.admin)
        .await;
    let (proxy_id, sandbox_name) = fixture.proxy(principal).await;
    sqlx::query(
        "INSERT INTO slack_bot_channels \
            (team_id, channel_id, name, bot_user_id, created_at, updated_at) \
         VALUES ('T-console-home', $1, 'general', 'U-bot', now(), now())",
    )
    .bind(fixture.name("channel"))
    .execute(&fixture.pool)
    .await
    .unwrap();

    let sandbox_jwt = SandboxJwtConfig::new(
        JWT_SECRET,
        crate::auth::SANDBOX_TOKEN_AUDIENCE,
        crate::auth::SANDBOX_TOKEN_ISSUER,
    )
    .unwrap();
    let state = AppState::unready(ApiAuthConfig::testing("test-secret"))
        .with_console_write_db(fixture.pool.clone())
        .with_console_session(
            ConsoleSessionConfig::from_settings(
                Some(SESSION_SECRET),
                Some("https://console.example.test"),
            )
            .unwrap()
            .unwrap()
            .with_sandbox_jwt(sandbox_jwt),
        );
    let app = Router::new()
        .nest(
            "/api/v1/sandbox",
            test_sandbox_routes(GuardedRoutes::new(state.clone())).into_router(),
        )
        .with_state(state);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let proxy_oid = oid("prx", proxy_id);
    let principal_oid = oid("prn", principal);

    let token = sandbox_token(
        JWT_SECRET,
        &sandbox_claims(&proxy_oid, &sandbox_name, &principal_oid, now),
    );
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/sandbox/probe/sandbox",
            &[("authorization", &format!("Bearer {token}"))],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_body(response).await["sandbox_id"], sandbox_name);

    let wrong_principal = sandbox_claims(
        &proxy_oid,
        &sandbox_name,
        &oid("prn", principal + 1_000_000),
        now,
    );
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/sandbox/probe/sandbox",
            &[(
                "authorization",
                &format!("Bearer {}", sandbox_token(JWT_SECRET, &wrong_principal)),
            )],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(response).await["error"]["message"],
        "invalid sandbox token"
    );

    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/sandbox/probe/sandbox",
            &[],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(response).await["error"]["message"],
        "invalid or missing sandbox token"
    );

    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/sandbox/probe/sandbox",
            &[("authorization", "Bearer not-a-jwt")],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        json_body(response).await["error"]["message"],
        "invalid or missing sandbox token"
    );
}

#[tokio::test]
async fn console_session_db_codemode_token_skips_proxy_lookup_and_rechecks_principal() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let principal = fixture
        .principal("slack_dm", Some("T-codemode-home"), fixture.admin)
        .await;
    sqlx::query(
        "INSERT INTO slack_bot_channels \
            (team_id, channel_id, name, bot_user_id, created_at, updated_at) \
         VALUES ('T-codemode-home', $1, 'general', 'U-bot', now(), now())",
    )
    .bind(fixture.name("channel"))
    .execute(&fixture.pool)
    .await
    .unwrap();
    let state = AppState::unready(ApiAuthConfig::testing("test-secret"))
        .with_console_db(fixture.pool.clone())
        .with_console_write_db(fixture.pool.clone())
        .with_console_session(
            ConsoleSessionConfig::from_settings(
                Some(SESSION_SECRET),
                Some("https://console.example.test"),
            )
            .unwrap()
            .unwrap()
            .with_sandbox_jwt(
                SandboxJwtConfig::new(
                    JWT_SECRET,
                    crate::auth::SANDBOX_TOKEN_AUDIENCE,
                    crate::auth::SANDBOX_TOKEN_ISSUER,
                )
                .unwrap(),
            ),
        );
    let probe_app = Router::new()
        .nest(
            "/api/v1/sandbox",
            test_sandbox_routes(GuardedRoutes::new(state.clone())).into_router(),
        )
        .with_state(state.clone());
    let app = build_router_with_app_state(state);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let principal_oid = oid("prn", principal);
    let codemode_claims = |claims: &mut Value| {
        claims["binding"] = json!("codemode");
        claims["exp"] = json!(now + 900);
        sandbox_token(JWT_SECRET, claims)
    };

    // No proxy exists for this token: the guard skips the proxy lookup and
    // accepts the live principal. The delegated Console route still works.
    let mut claims = sandbox_claims("", "codemode", &principal_oid, now);
    let auth = format!("Bearer {}", codemode_claims(&mut claims));
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/me",
            &[("authorization", &auth)],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let me = json_body(response).await;
    assert_eq!(me["data"]["auth_method"], "sandbox");
    assert_eq!(me["data"]["user"]["id"], oid("usr", fixture.admin));

    // Longer lifetimes, a non-empty proxy id, and an unknown principal are all
    // rejected.
    let mut too_long = sandbox_claims("", "codemode", &principal_oid, now);
    too_long["binding"] = json!("codemode");
    too_long["exp"] = json!(now + 901);
    let mut with_proxy = sandbox_claims("prx_missing", "codemode", &principal_oid, now);
    let mut unknown = sandbox_claims("", "codemode", &oid("prn", principal + 1_000_000), now);
    for (case, claims) in [
        ("long lifetime", sandbox_token(JWT_SECRET, &too_long)),
        ("proxy id", codemode_claims(&mut with_proxy)),
        ("unknown principal", codemode_claims(&mut unknown)),
    ] {
        let response = send(
            &probe_app,
            request(
                Method::GET,
                "/api/v1/sandbox/probe/sandbox",
                &[("authorization", &format!("Bearer {claims}"))],
                Body::empty(),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{case}");
        assert_eq!(
            json_body(response).await["error"]["message"],
            "invalid sandbox token",
            "{case}"
        );
    }

    // The principal check is live: removing the principal revokes the token.
    sqlx::query("DELETE FROM principals WHERE id = $1")
        .bind(principal)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let response = send(
        &probe_app,
        request(
            Method::GET,
            "/api/v1/sandbox/probe/sandbox",
            &[("authorization", &auth)],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn console_session_db_sandbox_admin_jwt_acts_as_console_user() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let principal = fixture
        .principal("slack_dm", Some("T-console-home"), fixture.admin)
        .await;
    let (proxy_id, sandbox_name) = fixture.proxy(principal).await;
    sqlx::query(
        "INSERT INTO slack_bot_channels \
            (team_id, channel_id, name, bot_user_id, created_at, updated_at) \
         VALUES ('T-console-home', $1, 'general', 'U-bot', now(), now())",
    )
    .bind(fixture.name("channel"))
    .execute(&fixture.pool)
    .await
    .unwrap();

    let state = AppState::unready(ApiAuthConfig::testing("test-secret"))
        .with_console_db(fixture.pool.clone())
        .with_console_write_db(fixture.pool.clone())
        .with_console_session(
            ConsoleSessionConfig::from_settings(
                Some(SESSION_SECRET),
                Some("https://console.example.test"),
            )
            .unwrap()
            .unwrap()
            .with_sandbox_jwt(
                SandboxJwtConfig::new(
                    JWT_SECRET,
                    crate::auth::SANDBOX_TOKEN_AUDIENCE,
                    crate::auth::SANDBOX_TOKEN_ISSUER,
                )
                .unwrap(),
            ),
        );
    let app = build_router_with_app_state(state);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let token = sandbox_token(
        JWT_SECRET,
        &sandbox_claims(
            &oid("prx", proxy_id),
            &sandbox_name,
            &oid("prn", principal),
            now,
        ),
    );
    let auth = format!("Bearer {token}");

    // The delegated credential reads an admin-only console route without a
    // cookie, Origin, or CSRF token.
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/roles",
            &[("authorization", &auth)],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(json_body(response).await["data"].is_array());

    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/me",
            &[("authorization", &auth)],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(cookie(&response).is_none());
    let me = json_body(response).await;
    assert_eq!(me["data"]["auth_method"], "sandbox");
    assert_eq!(me["data"]["csrf_token"], Value::Null);
    assert_eq!(me["data"]["user"]["id"], oid("usr", fixture.admin));
    assert_eq!(me["data"]["nav"]["api_keys"], false);

    // A key outlives the delegation, so a sandbox cannot mint one.
    let response = send(
        &app,
        request(
            Method::POST,
            "/api/v1/console/api_keys",
            &[
                ("authorization", &auth),
                ("content-type", "application/json"),
            ],
            Body::from(r#"{"data":{"name":"from-a-sandbox"}}"#),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = send(
        &app,
        request(
            Method::DELETE,
            "/api/v1/console/session",
            &[("authorization", &auth)],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(cookie(&response).is_none());

    // A write needs neither a cookie nor a CSRF header either.
    let foreign_id = fixture.name("delegated");
    let body = json!({"data": {"foreign_id": foreign_id, "kind": "workflow"}});
    let response = send(
        &app,
        request(
            Method::POST,
            "/api/v1/console/principals",
            &[
                ("authorization", &auth),
                ("content-type", "application/json"),
            ],
            Body::from(body.to_string()),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // A sandbox may point a secret at an
    // external store, but never supply an inline value or an OAuth credential,
    // never edit an OAuth-backed secret, and never delete a secret.
    let secret_body = |source: Value| {
        json!({"data": {
            "foreign_id": fixture.name("delegated-secret"),
            "name": "Delegated secret",
            "kind": "custom",
            "inject_config": {"query_param": "delegated"},
            "source": source,
            "rules": [{"host": "delegated.example.test", "http_methods": ["GET"], "paths": ["/"]}],
        }})
    };
    let delegated_write = |method: Method, path: String, body: Value| {
        request(
            method,
            &path,
            &[
                ("authorization", &auth),
                ("content-type", "application/json"),
            ],
            Body::from(body.to_string()),
        )
    };
    for source_type in ["control_plane", "token_broker"] {
        let response = send(
            &app,
            delegated_write(
                Method::POST,
                "/api/v1/console/static_secrets".to_owned(),
                secret_body(json!({"source_type": source_type, "secret": "inline-value"})),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{source_type}");
        assert_eq!(
            json_body(response).await["error"]["message"],
            format!(
                "{source_type} sources cannot be set from a sandbox; reference an external store"
            )
        );
    }
    let response = send(
        &app,
        delegated_write(
            Method::POST,
            "/api/v1/console/static_secrets".to_owned(),
            secret_body(json!({"source_type": "env", "config": {"var": "DELEGATED_SECRET"}})),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let secret_oid = json_body(response).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = send(
        &app,
        delegated_write(
            Method::PATCH,
            format!("/api/v1/console/static_secrets/{secret_oid}"),
            json!({"data": {"source": {"source_type": "control_plane", "secret": "inline-value"}}}),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = send(
        &app,
        request(
            Method::DELETE,
            &format!("/api/v1/console/static_secrets/{secret_oid}"),
            &[("authorization", &auth)],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // A bearer API key is still not a console credential.
    let api_key = fixture
        .api_key(fixture.admin, &fixture.name("token"), false)
        .await;
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/roles",
            &[("authorization", &format!("Bearer {api_key}"))],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(response).await["error"]["details"]["reason"],
        "session_required"
    );

    // The session-only admin actions are not part of `operator.api`: the same
    // API key cannot read system settings or approve users.
    for path in [
        "/api/v1/console/system_settings",
        &format!(
            "/api/v1/console/users/{}/approve",
            oid("usr", fixture.admin)
        ),
    ] {
        let method = if path.ends_with("/approve") {
            Method::POST
        } else {
            Method::GET
        };
        let response = send(
            &app,
            request(
                method,
                path,
                &[("authorization", &format!("Bearer {api_key}"))],
                Body::empty(),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        assert_eq!(
            json_body(response).await["error"]["details"]["reason"],
            "session_required",
            "{path}"
        );
    }

    // A valid sandbox token for a non-DM principal is not a console credential.
    let channel_principal = fixture
        .principal("slack_channel", Some("T-console-home"), fixture.admin)
        .await;
    let (channel_proxy, channel_sandbox) = fixture.proxy(channel_principal).await;
    let channel_token = sandbox_token(
        JWT_SECRET,
        &sandbox_claims(
            &oid("prx", channel_proxy),
            &channel_sandbox,
            &oid("prn", channel_principal),
            now,
        ),
    );
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/roles",
            &[("authorization", &format!("Bearer {channel_token}"))],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // The internal service host carries the same delegated credential but not
    // the cookie-authenticated console surface.
    const INTERNAL_HOST: &str = "centaur-api.centaur.svc.cluster.local";
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/roles",
            &[("host", INTERNAL_HOST), ("authorization", &auth)],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let session_config = ConsoleSessionConfig::from_settings(
        Some(SESSION_SECRET),
        Some("https://console.example.test"),
    )
    .unwrap()
    .unwrap();
    let session = session_config
        .cookie()
        .set_cookie_header(&crate::console::session_cookie::ConsoleSession::signed_in(
            fixture.admin,
        ))
        .unwrap();
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/roles",
            &[
                ("host", INTERNAL_HOST),
                ("cookie", session.to_str().unwrap()),
            ],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn console_session_db_sandbox_delegate_is_rechecked_live() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let principal = fixture
        .principal("slack_dm", Some("T-console-home"), fixture.admin)
        .await;
    let (proxy_id, sandbox_name) = fixture.proxy(principal).await;
    sqlx::query(
        "INSERT INTO slack_bot_channels \
            (team_id, channel_id, name, bot_user_id, created_at, updated_at) \
         VALUES ('T-console-home', $1, 'general', 'U-bot', now(), now())",
    )
    .bind(fixture.name("channel"))
    .execute(&fixture.pool)
    .await
    .unwrap();

    let state = AppState::unready(ApiAuthConfig::testing("test-secret"))
        .with_console_db(fixture.pool.clone())
        .with_console_write_db(fixture.pool.clone())
        .with_console_session(
            ConsoleSessionConfig::from_settings(
                Some(SESSION_SECRET),
                Some("https://console.example.test"),
            )
            .unwrap()
            .unwrap()
            .with_sandbox_jwt(
                SandboxJwtConfig::new(
                    JWT_SECRET,
                    crate::auth::SANDBOX_TOKEN_AUDIENCE,
                    crate::auth::SANDBOX_TOKEN_ISSUER,
                )
                .unwrap(),
            ),
        );
    let app = build_router_with_app_state(state);
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let token = sandbox_token(
        JWT_SECRET,
        &sandbox_claims(
            &oid("prx", proxy_id),
            &sandbox_name,
            &oid("prn", principal),
            now,
        ),
    );
    let auth = format!("Bearer {token}");

    let response = send_console_roles(&app, &auth).await;
    assert_eq!(response.status(), StatusCode::OK);

    // Demoting the linked admin revokes the delegated Console capability on the
    // next request even though the token still verifies.
    sqlx::query("UPDATE users SET admin = false WHERE id = $1")
        .bind(fixture.admin)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let response = send_console_roles(&app, &auth).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(response).await["error"]["details"]["reason"],
        "session_required"
    );

    // Re-promoting but disabling the account does the same.
    sqlx::query("UPDATE users SET admin = true, status = 'disabled' WHERE id = $1")
        .bind(fixture.admin)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let response = send_console_roles(&app, &auth).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // The proxy binding is re-read too: reassigning it invalidates the token.
    sqlx::query("UPDATE users SET status = 'active' WHERE id = $1")
        .bind(fixture.admin)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let other = fixture
        .principal("other_dm", Some("T-console-home"), fixture.admin)
        .await;
    sqlx::query("UPDATE proxies SET principal_id = $1 WHERE id = $2")
        .bind(other)
        .bind(proxy_id)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let response = send_console_roles(&app, &auth).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        json_body(response).await["error"]["details"]["reason"],
        "session_required"
    );
}

/// The workflow runtime's in-process force-start path, against a real
/// `WorkflowRuntime` and the absurd queue tables: the handler queues through
/// the runtime production uses.
#[tokio::test]
async fn console_session_db_workflow_force_start_uses_in_process_runtime() {
    use std::sync::Arc;

    use async_trait::async_trait;
    use centaur_iron_control::{IronControlClient, IronControlError, Principal};
    use centaur_sandbox_core::{
        ObservedSandbox, SandboxBackend, SandboxError, SandboxHandle, SandboxId, SandboxIo,
        SandboxResult, SandboxSpec, SandboxStatus,
    };
    use centaur_session_runtime::{SandboxRuntime, SessionPrincipalRegistrar, SessionRuntime};
    use centaur_session_sqlx::PgSessionStore;
    use centaur_workflows::{
        WorkflowDiscoveryClient, WorkflowHostSandboxRuntime, WorkflowPrincipalRegistrar,
        WorkflowRuntime,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    let Some(fixture) = Fixture::new().await else {
        return;
    };

    let queues_exist: bool = sqlx::query_scalar("select to_regclass('absurd.queues') is not null")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    if !queues_exist {
        sqlx::raw_sql(include_str!(
            "../../../centaur-session-sqlx/migrations/0007_absurd_workflows.sql"
        ))
        .execute(&fixture.pool)
        .await
        .expect("install absurd schema in the test database");
    }

    struct TestBackend;
    #[async_trait]
    impl SandboxBackend for TestBackend {
        fn name(&self) -> &'static str {
            "test"
        }

        async fn create(&self, _spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
            Ok(SandboxHandle::new(
                SandboxId::new("test-sandbox"),
                self.name(),
            ))
        }

        async fn open_io(&self, _id: &SandboxId) -> SandboxResult<SandboxIo> {
            Err(SandboxError::Unsupported {
                backend: self.name(),
                operation: "open_io",
            })
        }

        async fn status(&self, _id: &SandboxId) -> SandboxResult<SandboxStatus> {
            Ok(SandboxStatus::Running)
        }

        async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
            Ok(ObservedSandbox::new(
                id.clone(),
                self.name(),
                SandboxStatus::Running,
            ))
        }

        async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
            Ok(Vec::new())
        }

        async fn stop(&self, _id: &SandboxId) -> SandboxResult<()> {
            Ok(())
        }

        async fn pause(&self, _id: &SandboxId) -> SandboxResult<()> {
            Err(SandboxError::Unsupported {
                backend: self.name(),
                operation: "pause",
            })
        }

        async fn resume(&self, _id: &SandboxId) -> SandboxResult<()> {
            Err(SandboxError::Unsupported {
                backend: self.name(),
                operation: "resume",
            })
        }
    }

    struct TestRegistrar;
    #[async_trait]
    impl SessionPrincipalRegistrar for TestRegistrar {
        async fn register_session(
            &self,
            _thread_key: &str,
            _metadata: Option<&Value>,
            _create_if_missing: bool,
        ) -> Result<Principal, IronControlError> {
            Err(IronControlError::NotFound {
                resource: "test principal",
                ident: "force-start".to_owned(),
            })
        }

        async fn register_requester(
            &self,
            _thread_key: &str,
            _metadata: Option<&Value>,
            _create_if_missing: bool,
        ) -> Result<Option<Principal>, IronControlError> {
            Ok(None)
        }

        async fn get_principal(&self, principal: &str) -> Result<Principal, IronControlError> {
            Err(IronControlError::NotFound {
                resource: "test principal",
                ident: principal.to_owned(),
            })
        }
    }

    // The workflow runtime needs a discovery host. An empty discovery is all
    // this route needs and keeps python dependencies out of the test.
    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let temp = TempDir(std::env::temp_dir().join(format!("centaur-workflow-{}", fixture.unique)));
    let workflows_dir = temp.0.join("workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();
    let host_path = temp.0.join("workflow_host.py");
    std::fs::write(
        &host_path,
        "import json, sys\nfor line in sys.stdin:\n    message = json.loads(line)\n    if message.get('type') == 'workflow.discover':\n        print(json.dumps({'type': 'workflow.discovery', 'workflows': []}), flush=True)\n        break\n",
    )
    .unwrap();

    let _env_guard = ENV_LOCK.lock().await;
    let names = [
        "PYTHON_WORKFLOW_HOST_PATH",
        "WORKFLOW_DIRS",
        "WORKFLOW_ENABLE_MODE",
        "WORKFLOW_RECONCILE_INTERVAL_SECS",
    ];
    let saved: Vec<_> = names
        .iter()
        .map(|name| (*name, std::env::var_os(name)))
        .collect();
    // SAFETY: ENV_LOCK serializes this test's process-environment mutation
    // with itself; the restored values are applied before the guard drops.
    unsafe {
        std::env::set_var("PYTHON_WORKFLOW_HOST_PATH", &host_path);
        std::env::set_var("WORKFLOW_DIRS", &workflows_dir);
        std::env::set_var("WORKFLOW_ENABLE_MODE", "all");
        std::env::set_var("WORKFLOW_RECONCILE_INTERVAL_SECS", "0");
    }

    let store = PgSessionStore::new(fixture.pool.clone());
    let session_runtime = SessionRuntime::new(
        store.clone(),
        SandboxRuntime::backend(Arc::new(TestBackend), SandboxSpec::new("test")),
        TestRegistrar,
    );
    // Workflow discovery runs in its own service; answer startup discovery with
    // an empty catalog so the runtime comes up without one.
    let discovery_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let discovery_url = format!("http://{}", discovery_listener.local_addr().unwrap());
    let discovery_server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = discovery_listener.accept().await else {
                return;
            };
            let mut buffer = vec![0u8; 16 * 1024];
            let _ = stream.read(&mut buffer).await;
            let body = r#"{"workflows":[]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    let workflows = WorkflowRuntime::new(
        store,
        session_runtime.clone(),
        WorkflowHostSandboxRuntime::new(
            SandboxRuntime::backend(Arc::new(TestBackend), SandboxSpec::new("test")),
            SandboxSpec::new("workflow-host"),
        ),
        WorkflowDiscoveryClient::new(
            &discovery_url,
            &workflows_dir.to_string_lossy(),
            std::collections::BTreeMap::new(),
        ),
        WorkflowPrincipalRegistrar::new(IronControlClient::new(fixture.pool.clone())),
    )
    .await
    .expect("build the real workflow runtime");

    let state = AppState::unready(ApiAuthConfig::testing("test-secret"))
        .with_console_db(fixture.pool.clone())
        .with_console_write_db(fixture.pool.clone())
        .with_console_session(
            ConsoleSessionConfig::from_settings(
                Some(SESSION_SECRET),
                Some("https://console.example.test"),
            )
            .unwrap()
            .unwrap(),
        );
    state.mark_ready(session_runtime, Some(workflows), Some(fixture.pool.clone()));
    let app = build_router_with_app_state(state);

    let response = login(&app, &fixture.admin_email, PASSWORD).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let signed_in_cookie = cookie(&response).expect("admin signs in");
    let response = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/me",
            &[("cookie", signed_in_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    let me_cookie = cookie(&response).unwrap_or_else(|| signed_in_cookie.clone());
    let _me = json_body(response).await;

    let workflow_name = fixture.name("force-start");
    let response = send(
        &app,
        request(
            Method::POST,
            &format!("/api/v1/console/workflows/{workflow_name}/runs"),
            &[origin(), ("cookie", me_cookie.as_str())],
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = json_body(response).await;
    assert_eq!(body["data"]["created"], true);
    let run_id = body["data"]["run_id"].as_str().unwrap();

    let queued_task: bool = sqlx::query_scalar(
        "select exists(select 1 from absurd.t_centaur_workflows \
         where params->>'workflow_name' = $1)",
    )
    .bind(&workflow_name)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert!(queued_task, "the runtime queued the force-started workflow");
    let queued_run: bool = sqlx::query_scalar(
        "select exists(select 1 from absurd.r_centaur_workflows where run_id = $1::uuid)",
    )
    .bind(run_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert!(queued_run, "the response names a queued absurd run");
    discovery_server.abort();

    for (name, value) in saved {
        // SAFETY: see the set block above; the guard still holds the lock.
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

/// Every list, role-grant, and sandbox-permission body carries exactly one
/// `data` envelope. A helper that wraps an already-enveloped value produced
/// `{"data":{"data":...}}`, which the SPA and the connector cannot read.
#[tokio::test]
async fn console_session_db_responses_have_exactly_one_data_envelope() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let secret_id: i64 = sqlx::query_scalar(
        "INSERT INTO static_secrets (name, foreign_id, labels, created_by_id, created_at, updated_at) \
         VALUES ($1, $2, '{}', $3, now(), now()) RETURNING id",
    )
    .bind(fixture.name("envelope-secret"))
    .bind(fixture.name("envelope-secret-foreign"))
    .bind(fixture.admin)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let principal = fixture
        .principal("slack_dm", Some("T-envelope"), fixture.admin)
        .await;
    let (proxy_id, sandbox_name) = fixture.proxy(principal).await;

    let sandbox_jwt = SandboxJwtConfig::new(
        JWT_SECRET,
        crate::auth::SANDBOX_TOKEN_AUDIENCE,
        crate::auth::SANDBOX_TOKEN_ISSUER,
    )
    .unwrap();
    let state = AppState::unready(ApiAuthConfig::testing("test-secret"))
        .with_console_db(fixture.pool.clone())
        .with_console_write_db(fixture.pool.clone())
        .with_console_session(
            ConsoleSessionConfig::from_settings(
                Some(SESSION_SECRET),
                Some("https://console.example.test"),
            )
            .unwrap()
            .unwrap()
            .with_sandbox_jwt(sandbox_jwt),
        );
    let app = build_router_with_app_state(state);

    let response = login(&app, &fixture.admin_email, PASSWORD).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let session = cookie(&response).expect("admin signs in");
    let get = |uri: String, auth: (&'static str, String)| {
        let app = app.clone();
        async move {
            let response = send(
                &app,
                request(
                    Method::GET,
                    &uri,
                    &[(auth.0, auth.1.as_str())],
                    Body::empty(),
                ),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            json_body(response).await
        }
    };

    let list = get(
        "/api/v1/console/secrets?limit=200".to_owned(),
        ("cookie", session.clone()),
    )
    .await;
    assert!(list["data"].is_array(), "{list}");
    assert!(list["meta"]["total"].is_i64(), "{list}");

    let grants = get(
        format!(
            "/api/v1/console/secrets/static/{}/role_grants",
            oid("ssr", secret_id)
        ),
        ("cookie", session.clone()),
    )
    .await;
    assert!(grants["data"].is_array(), "{grants}");
    assert!(grants["assignable_roles"].is_array(), "{grants}");

    let token = format!("fake-api-key-envelope-{}", fixture.unique);
    fixture.api_key(fixture.admin, &token, false).await;
    let bearer = get(
        "/api/v1/static_secrets?limit=200".to_owned(),
        ("authorization", format!("Bearer {token}")),
    )
    .await;
    assert!(bearer["data"].is_array(), "{bearer}");
    assert!(bearer["meta"]["total"].is_i64(), "{bearer}");

    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let sandbox = sandbox_token(
        JWT_SECRET,
        &sandbox_claims(
            &oid("prx", proxy_id),
            &sandbox_name,
            &oid("prn", principal),
            now,
        ),
    );
    let permissions = get(
        "/api/v1/sandbox/permissions".to_owned(),
        ("authorization", format!("Bearer {sandbox}")),
    )
    .await;
    assert_eq!(
        permissions["data"]["sandbox_id"], sandbox_name,
        "{permissions}"
    );
    assert!(permissions["data"].get("data").is_none(), "{permissions}");
}
