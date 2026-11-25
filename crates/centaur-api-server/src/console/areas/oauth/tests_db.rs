//! Database-backed tests for the OAuth area.
//!
//! They drive the real routers with a Console session or API key, so the
//! guards, JSON envelopes, validation details, encrypted writes, delete
//! guards, and the consent flow all exercise the same code as production.
//! Without `CONSOLE_DB_TEST_DATABASE_URL` they skip locally and fail in CI.

use std::env;
use std::sync::Once;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode, header},
    response::Response,
};
use centaur_console_db::{areas::oauth::OauthEncryption, decode_oid, oid};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tower::ServiceExt;
use url::Url;

use crate::{
    ApiAuthConfig, AppState, build_router_with_app_state, console::tests_db::test_pool,
    console_session::ConsoleSessionConfig,
};

const SESSION_SECRET: &str =
    "test-only-console-session-secret-0000000000000000000000000000000000000000000000000000";
const PUBLIC_URL: &str = "https://console.example.test";
const CONSOLE_HOST: &str = "console.example.test";
const AR_KEY: &str = "oauth-area-test-encryption-primary-key";
const AR_SALT: &str = "oauth-area-test-encryption-salt";

static ENV: Once = Once::new();

fn init_env() {
    ENV.call_once(|| {
        // SAFETY: this runs once, before any test in this module reads these
        // variables, and every value is stable for the process lifetime.
        unsafe {
            env::set_var("CENTAUR_CONSOLE_PUBLIC_URL", PUBLIC_URL);
            env::set_var("CENTAUR_CONSOLE_AR_ENCRYPTION_PRIMARY_KEY", AR_KEY);
            env::set_var("CENTAUR_CONSOLE_AR_ENCRYPTION_KEY_DERIVATION_SALT", AR_SALT);
        }
    });
}

fn encryption() -> OauthEncryption {
    OauthEncryption::new(AR_KEY, AR_SALT)
}

fn unique() -> String {
    let mut bytes = [0; 12];
    getrandom::fill(&mut bytes).expect("operating system randomness");
    hex::encode(bytes)
}

struct Fixture {
    pool: PgPool,
    unique: String,
    admin: i64,
    member: i64,
    api_token: String,
}

impl Fixture {
    async fn new() -> Option<Self> {
        init_env();
        let pool = test_pool().await?;
        let unique = unique();
        let admin = insert_user(&pool, &format!("oauth-admin-{unique}@example.test"), true).await;
        let member =
            insert_user(&pool, &format!("oauth-member-{unique}@example.test"), false).await;
        let api_token = format!("iak_oauth_area_test_token_{unique}");
        insert_api_key(&pool, admin, &api_token, unique.as_str()).await;
        Some(Self {
            pool,
            unique,
            admin,
            member,
            api_token,
        })
    }

    fn slug(&self, label: &str) -> String {
        format!("{label}-{}", self.unique)
    }

    async fn app(&self, slug: &str, provider: &str, shared: bool, enabled: bool) -> i64 {
        insert_app(
            &self.pool,
            slug,
            provider,
            shared,
            enabled,
            &["read".to_owned()],
            &serde_json::Map::new(),
        )
        .await
    }
}

async fn insert_user(pool: &PgPool, email: &str, admin: bool) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO users (email, admin, status, created_at, updated_at) \
         VALUES ($1, $2, 'active', now(), now()) RETURNING id",
    )
    .bind(email)
    .bind(admin)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn insert_api_key(pool: &PgPool, user_id: i64, token: &str, name: &str) {
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    sqlx::query(
        "INSERT INTO api_keys (name, token_hash, user_id, created_at, updated_at) \
         VALUES ($1, $2, $3, now(), now())",
    )
    .bind(format!("oauth-test-{name}"))
    .bind(token_hash)
    .bind(user_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_app(
    pool: &PgPool,
    slug: &str,
    provider: &str,
    shared: bool,
    enabled: bool,
    scopes: &[String],
    provider_config: &serde_json::Map<String, Value>,
) -> i64 {
    let secret = encryption().encrypt("client-secret").unwrap();
    sqlx::query_scalar(
        "INSERT INTO oauth_apps \
         (slug, description, labels, provider, client_id, client_secret, allowed_scopes, \
          provider_config, enabled, always_available, shared, owner_only, created_by_id, \
          created_at, updated_at) \
         VALUES ($1, 'Example app', '{}', $2, 'client-id', $3, $4, $5, $6, FALSE, $7, FALSE, \
                 (SELECT id FROM users WHERE admin ORDER BY id LIMIT 1), now(), now()) \
         RETURNING id",
    )
    .bind(slug)
    .bind(provider)
    .bind(secret)
    .bind(serde_json::Value::Array(
        scopes.iter().cloned().map(Value::String).collect(),
    ))
    .bind(serde_json::Value::Object(provider_config.clone()))
    .bind(enabled)
    .bind(shared)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn insert_credential(
    pool: &PgPool,
    app_id: i64,
    subject: &str,
    created_by: i64,
    email: Option<&str>,
) -> i64 {
    let encryption = encryption();
    sqlx::query_scalar(
        "INSERT INTO broker_credentials \
         (oauth_app_id, foreign_id, name, token_endpoint, provider_subject, provider_email, \
          external_user_key, created_by_id, access_token, refresh_token, scopes, labels, \
          expires_at, last_refresh, next_attempt_at, failure_count, dead, created_at, updated_at) \
         VALUES ($1, $2, 'Example credential', 'https://auth.example.test/token', $3, $4, \
                 'user-key', $5, $6, $7, '[\"read\"]', '{}', now() + interval '1 hour', now(), \
                 now(), 0, FALSE, now(), now()) \
         RETURNING id",
    )
    .bind(app_id)
    .bind(format!("cred-{subject}"))
    .bind(subject)
    .bind(email)
    .bind(created_by)
    .bind(encryption.encrypt("access-token").unwrap())
    .bind(encryption.encrypt("refresh-token").unwrap())
    .fetch_one(pool)
    .await
    .unwrap()
}

fn config() -> ConsoleSessionConfig {
    ConsoleSessionConfig::from_settings(Some(SESSION_SECRET), Some(PUBLIC_URL))
        .unwrap()
        .unwrap()
}

fn app(fixture: &Fixture) -> Router {
    let state = AppState::unready(ApiAuthConfig::testing("test-secret"))
        .with_console_db(fixture.pool.clone())
        .with_console_write_db(fixture.pool.clone())
        .with_console_session(config());
    build_router_with_app_state(state)
}

fn session_cookie(user_id: i64) -> String {
    config()
        .cookie()
        .set_cookie_header(&crate::console::session_cookie::ConsoleSession::signed_in(
            user_id,
        ))
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

fn request(
    method: Method,
    uri: &str,
    headers: &[(&str, String)],
    body: Option<Value>,
) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"))
    {
        request = request.header(header::HOST, CONSOLE_HOST);
    }
    for (name, value) in headers {
        request = request.header(*name, value);
    }
    match body {
        Some(body) => request
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
        None => request.body(Body::empty()).unwrap(),
    }
}

fn console_headers(user_id: i64) -> Vec<(&'static str, String)> {
    let session = crate::console::session_cookie::ConsoleSession::signed_in(user_id);
    let cookie = config()
        .cookie()
        .set_cookie_header(&session)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    vec![
        (header::COOKIE.as_str(), cookie),
        ("origin", PUBLIC_URL.to_owned()),
    ]
}

fn bearer_headers(token: &str) -> Vec<(&'static str, String)> {
    vec![("authorization", format!("Bearer {token}"))]
}

async fn send(app: &Router, request: Request<Body>) -> Response {
    app.clone().oneshot(request).await.unwrap()
}

async fn json_body(response: Response) -> Value {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(!bytes.is_empty(), "expected a body, status was {status}");
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn console_oauth_apps_crud_validates_and_guards_deletes() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let app = app(&fixture);
    let slug = fixture.slug("console-app");
    let headers = console_headers(fixture.admin);

    let list = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/oauth_apps?limit=200",
            &headers,
            None,
        ),
    )
    .await;
    assert_eq!(list.status(), StatusCode::OK);
    let body = json_body(list).await;
    assert!(body["data"].is_array());

    let create = send(
        &app,
        request(
            Method::POST,
            "/api/v1/console/oauth_apps",
            &headers,
            Some(json!({"data": {
                "slug": slug,
                "provider": "google",
                "client_id": "created-client-id",
                "client_secret": "created-secret",
                "allowed_scopes": ["scope.read"],
                "labels": {"team": "oauth-test"}
            }})),
        ),
    )
    .await;
    let create_status = create.status();
    let created = json_body(create).await;
    assert_eq!(create_status, StatusCode::CREATED, "{created}");
    assert_eq!(created["data"]["slug"], slug);
    assert_eq!(created["data"]["client_secret_set"], true);
    let app_id = created["data"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        created["data"]["redirect_uri"],
        format!("{PUBLIC_URL}/oauth/{slug}/callback")
    );
    let row: (String, String) =
        sqlx::query_as("SELECT client_secret, provider FROM oauth_apps WHERE slug = $1")
            .bind(&slug)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(row.1, "google");
    assert_ne!(row.0, "created-secret");
    assert_eq!(encryption().decrypt(&row.0).unwrap(), "created-secret");

    let duplicate = send(
        &app,
        request(
            Method::POST,
            "/api/v1/console/oauth_apps",
            &headers,
            Some(json!({"data": {
                "slug": slug, "provider": "google", "client_id": "cid",
                "client_secret": "secret", "allowed_scopes": ["scope.read"]
            }})),
        ),
    )
    .await;
    assert_eq!(duplicate.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let duplicate = json_body(duplicate).await;
    assert_eq!(duplicate["error"]["message"], "validation failed");
    assert!(
        duplicate["error"]["details"]["slug"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message == "has already been taken")
    );

    // A shared app cannot become always available or un-shared.
    let shared_slug = fixture.slug("shared-app");
    let shared_id = fixture.app(&shared_slug, "google", true, true).await;
    let shared_patch = send(
        &app,
        request(
            Method::PATCH,
            &format!("/api/v1/console/oauth_apps/{}", oid("oap", shared_id)),
            &headers,
            Some(json!({"data": {"shared": false}})),
        ),
    )
    .await;
    assert_eq!(shared_patch.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let shared_patch = json_body(shared_patch).await;
    assert!(
        shared_patch["error"]["details"]["shared"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message == "cannot be changed after creation")
    );

    // Delete is blocked while an app has minted credentials, and succeeds
    // once they are gone.
    let credential = insert_credential(
        &fixture.pool,
        shared_id,
        &fixture.slug("sub"),
        fixture.member,
        None,
    )
    .await;
    let blocked = send(
        &app,
        request(
            Method::DELETE,
            &format!("/api/v1/console/oauth_apps/{}", oid("oap", shared_id)),
            &headers,
            None,
        ),
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::CONFLICT);
    sqlx::query("DELETE FROM broker_credentials WHERE id = $1")
        .bind(credential)
        .execute(&fixture.pool)
        .await
        .unwrap();
    let deleted = send(
        &app,
        request(
            Method::DELETE,
            &format!("/api/v1/console/oauth_apps/{}", oid("oap", shared_id)),
            &headers,
            None,
        ),
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);

    let deleted_created = send(
        &app,
        request(
            Method::DELETE,
            &format!("/api/v1/console/oauth_apps/{app_id}"),
            &headers,
            None,
        ),
    )
    .await;
    assert_eq!(deleted_created.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn broker_credentials_encrypt_write_only_values_and_merge_headers() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let app = app(&fixture);
    let headers = console_headers(fixture.admin);
    let foreign_id = fixture.slug("broker");

    let create = send(
        &app,
        request(
            Method::POST,
            "/api/v1/console/broker_credentials",
            &headers,
            Some(json!({"data": {
                "foreign_id": foreign_id,
                "name": "Example",
                "grant": "client_credentials",
                "token_endpoint": "https://auth.example.test/token",
                "client_id": "client-id",
                "client_secret": "broker-secret",
                "scopes": ["read"],
                "labels": {"team": "oauth-test"},
                "token_endpoint_headers": {"X-A": "a-value"}
            }})),
        ),
    )
    .await;
    let status = create.status();
    let body = json_body(create).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["has_client_secret"], true);
    assert_eq!(body["data"]["token_endpoint_header_names"], json!(["X-A"]));
    let id = body["data"]["id"].as_str().unwrap().to_owned();

    let row: (String, String) = sqlx::query_as(
        "SELECT client_secret, token_endpoint_headers FROM broker_credentials WHERE foreign_id = $1",
    )
    .bind(&foreign_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_ne!(row.0, "broker-secret");
    assert_eq!(encryption().decrypt(&row.0).unwrap(), "broker-secret");
    let stored_headers: Value =
        serde_json::from_str(&encryption().decrypt(&row.1).unwrap()).unwrap();
    assert_eq!(stored_headers, json!({"X-A": "a-value"}));

    let patch = send(
        &app,
        request(
            Method::PATCH,
            &format!("/api/v1/console/broker_credentials/{id}"),
            &headers,
            Some(json!({"data": {
                "description": "Renamed",
                "token_endpoint_headers": {"X-A": null, "X-B": "b-value"},
                "client_secret": ""
            }})),
        ),
    )
    .await;
    let status = patch.status();
    let body = json_body(patch).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["description"], "Renamed");
    assert_eq!(
        body["data"]["token_endpoint_header_names"],
        json!(["X-A", "X-B"])
    );
    let stored: String = sqlx::query_scalar(
        "SELECT token_endpoint_headers FROM broker_credentials WHERE foreign_id = $1",
    )
    .bind(&foreign_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let stored_headers: Value =
        serde_json::from_str(&encryption().decrypt(&stored).unwrap()).unwrap();
    assert_eq!(stored_headers, json!({"X-A": "a-value", "X-B": "b-value"}));

    let missing_grant = send(
        &app,
        request(
            Method::POST,
            "/api/v1/console/broker_credentials",
            &headers,
            Some(json!({"data": {
                "foreign_id": fixture.slug("bad-grant"),
                "grant": "nope",
                "token_endpoint": "https://auth.example.test/token",
                "client_id": "cid"
            }})),
        ),
    )
    .await;
    assert_eq!(missing_grant.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let missing_grant = json_body(missing_grant).await;
    assert!(
        missing_grant["error"]["details"]["grant"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message.as_str().unwrap().contains("must be one of"))
    );

    // A token_broker source still references the credential: delete is 409.
    let credential_id = decode_oid("bcr", &id).unwrap();
    sqlx::query(
        "INSERT INTO static_secrets (broker_credential_id, name, kind, inject_config, labels, created_at, updated_at) \
         VALUES ($1, 'wrapper', 'custom', '{\"header\":\"Authorization\"}', '{}', now(), now())",
    )
    .bind(credential_id)
    .execute(&fixture.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO secret_sources (static_secret_id, broker_credential_id, source_type, config, created_at, updated_at) \
         SELECT id, $1, 'token_broker', jsonb_build_object('credential_id', $2), now(), now() \
         FROM static_secrets WHERE broker_credential_id = $1",
    )
    .bind(credential_id)
    .bind(oid("bcr", credential_id))
    .execute(&fixture.pool)
    .await
    .unwrap();
    let blocked = send(
        &app,
        request(
            Method::DELETE,
            &format!("/api/v1/console/broker_credentials/{id}"),
            &headers,
            None,
        ),
    )
    .await;
    assert_eq!(blocked.status(), StatusCode::CONFLICT);
}

async fn spawn_mock(responses: Vec<String>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        for body in responses {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buffer = vec![0; 8192];
            let _ = socket.read(&mut buffer).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (format!("http://{address}"), handle)
}

fn set_cookie(response: &Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&format!("{name}=")))
        .map(|value| value.split(';').next().unwrap().to_owned())
}

fn location_state(response: &Response) -> String {
    let location = response.headers()[header::LOCATION].to_str().unwrap();
    let url = Url::parse(location).unwrap();
    url.query_pairs()
        .find(|(key, _)| key == "state")
        .map(|(_, value)| value.into_owned())
        .expect("authorization URL carries state")
}

#[tokio::test]
async fn consent_flow_start_callback_provider_error_and_success() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let app = app(&fixture);
    let admin_headers = console_headers(fixture.admin);
    let (base, handle) = spawn_mock(vec![
        json!({
            "access_token": "flow-access",
            "refresh_token": "flow-refresh",
            "expires_in": 3600,
            "scope": "read"
        })
        .to_string(),
        json!({
            "sub": "flow-subject",
            "email": "flow@example.test",
            "name": "Flow User"
        })
        .to_string(),
    ])
    .await;
    let slug = fixture.slug("flow");
    let mut provider_config = serde_json::Map::new();
    provider_config.insert(
        "authorization_endpoint".to_owned(),
        Value::String(format!("{base}/authorize")),
    );
    provider_config.insert(
        "token_endpoint".to_owned(),
        Value::String(format!("{base}/token")),
    );
    provider_config.insert(
        "userinfo_endpoint".to_owned(),
        Value::String(format!("{base}/userinfo")),
    );
    provider_config.insert(
        "resource".to_owned(),
        Value::String("https://api.example.test/mcp".to_owned()),
    );
    let app_id = insert_app(
        &fixture.pool,
        &slug,
        "custom",
        false,
        true,
        &["read".to_owned()],
        &provider_config,
    )
    .await;

    // Signed out, unknown, and disallowed scope paths.
    let signed_out = send(
        &app,
        request(Method::GET, &format!("/oauth/{slug}/start"), &[], None),
    )
    .await;
    assert_eq!(signed_out.status(), StatusCode::FOUND);
    assert_eq!(signed_out.headers()[header::LOCATION], "/login");

    let unknown = send(
        &app,
        request(
            Method::GET,
            "/oauth/missing-flow-app/start",
            &admin_headers,
            None,
        ),
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let disallowed = send(
        &app,
        request(
            Method::GET,
            &format!("/oauth/{slug}/start?scopes=write"),
            &admin_headers,
            None,
        ),
    )
    .await;
    assert_eq!(disallowed.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Start redirects to the configured provider with PKCE and a flow cookie.
    let start = send(
        &app,
        request(
            Method::GET,
            &format!("/oauth/{slug}/start?scopes=read"),
            &admin_headers,
            None,
        ),
    )
    .await;
    assert_eq!(start.status(), StatusCode::FOUND);
    let location = start.headers()[header::LOCATION].to_str().unwrap();
    let url = Url::parse(location).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(url.path(), "/authorize");
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["scope"], "read");
    assert_eq!(query["resource"], "https://api.example.test/mcp");
    let state = location_state(&start);
    let flow_cookie = set_cookie(&start, "oauth_flow").expect("flow cookie");

    // Provider-side denial renders the denied page and stores nothing.
    let denied = send(
        &app,
        request(
            Method::GET,
            &format!("/oauth/{slug}/callback?state={state}&error=access_denied"),
            &[(
                header::COOKIE.as_str(),
                format!("{}; {flow_cookie}", session_cookie(fixture.admin)),
            )],
            None,
        ),
    )
    .await;
    assert_eq!(denied.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // A fresh start plus a successful exchange creates the credential and its
    // wrapping secret.
    let start = send(
        &app,
        request(
            Method::GET,
            &format!("/oauth/{slug}/start?scopes=read"),
            &admin_headers,
            None,
        ),
    )
    .await;
    let state = location_state(&start);
    let flow_cookie = set_cookie(&start, "oauth_flow").expect("flow cookie");
    let callback = send(
        &app,
        request(
            Method::GET,
            &format!("/oauth/{slug}/callback?state={state}&code=authorization-code"),
            &[(
                header::COOKIE.as_str(),
                format!("{}; {flow_cookie}", session_cookie(fixture.admin)),
            )],
            None,
        ),
    )
    .await;
    assert_eq!(callback.status(), StatusCode::FOUND);
    assert_eq!(
        callback.headers()[header::LOCATION],
        "/console/integrations"
    );

    let credential_id: i64 =
        sqlx::query_scalar("SELECT id FROM broker_credentials WHERE oauth_app_id = $1")
            .bind(app_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    let row = sqlx::query(
        "SELECT provider_subject, provider_email, access_token, scopes, dead \
         FROM broker_credentials WHERE id = $1",
    )
    .bind(credential_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    let subject: Option<String> = row.try_get("provider_subject").unwrap();
    let email: Option<String> = row.try_get("provider_email").unwrap();
    let access_token: Option<String> = row.try_get("access_token").unwrap();
    let scopes: sqlx::types::Json<Vec<String>> = row.try_get("scopes").unwrap();
    let dead: bool = row.try_get("dead").unwrap();
    assert_eq!(subject.as_deref(), Some("flow-subject"));
    assert_eq!(email.as_deref(), Some("flow@example.test"));
    assert_eq!(
        encryption()
            .decrypt(access_token.as_deref().unwrap())
            .unwrap(),
        "flow-access"
    );
    assert_eq!(scopes.0, vec!["read".to_owned()]);
    assert!(!dead);

    let secret: (String, String, Value) = sqlx::query_as(
        "SELECT name, kind, inject_config FROM static_secrets WHERE broker_credential_id = $1",
    )
    .bind(credential_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert!(secret.0.ends_with(" token"));
    assert_eq!(secret.1, "custom");
    assert_eq!(secret.2["header"], "Authorization");
    let source_config: Value =
        sqlx::query_scalar("SELECT config FROM secret_sources WHERE broker_credential_id = $1")
            .bind(credential_id)
            .fetch_one(&fixture.pool)
            .await
            .unwrap();
    assert_eq!(source_config["credential_id"], oid("bcr", credential_id));

    let integrations = send(
        &app,
        request(
            Method::GET,
            "/api/v1/console/integrations",
            &admin_headers,
            None,
        ),
    )
    .await;
    let body = json_body(integrations).await;
    let entry = body["data"]["apps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|app| app["id"] == oid("oap", app_id))
        .unwrap();
    assert_eq!(entry["connection"]["status"], "connected");

    handle.abort();
}

#[tokio::test]
async fn slack_consent_enriches_identity_and_tolerates_provider_error() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let app = app(&fixture);
    let admin_headers = console_headers(fixture.admin);
    let (base, handle) = spawn_mock(vec![
        json!({
            "ok": true,
            "access_token": "xoxb-team-token",
            "authed_user": {
                "id": "U12345",
                "name": "Old Name",
                "access_token": "xoxp-user-token",
                "scope": "users:read,users:read.email"
            },
            "team": {"id": "T12345", "name": "Acme"},
            "scope": "users:read,users:read.email"
        })
        .to_string(),
        json!({"ok": true, "user": "fallback"}).to_string(),
        json!({
            "ok": true,
            "user": {"profile": {
                "display_name": "Grace Hopper",
                "email": "grace@example.com"
            }}
        })
        .to_string(),
        json!({
            "ok": true,
            "access_token": "xoxb-other-token",
            "authed_user": {
                "id": "U99999",
                "name": "Other User",
                "access_token": "xoxp-other-token",
                "scope": "users:read.email"
            },
            "team": {"id": "T99999", "name": "Other"},
            "scope": "users:read.email"
        })
        .to_string(),
        json!({"ok": false, "error": "invalid_auth"}).to_string(),
    ])
    .await;
    super::SLACK_API_BASE
        .set(base)
        .expect("set the Slack API mock base before the first Slack request");

    // A successful Slack consent sets the credential and its wrapper secret to
    // the profile Slack returns.
    let slug = fixture.slug("slack-enrich");
    let app_id = insert_app(
        &fixture.pool,
        &slug,
        "slack",
        false,
        true,
        &["users:read".to_owned(), "users:read.email".to_owned()],
        &serde_json::Map::new(),
    )
    .await;
    let start = send(
        &app,
        request(
            Method::GET,
            &format!("/oauth/{slug}/start?scopes=users:read,users:read.email"),
            &admin_headers,
            None,
        ),
    )
    .await;
    assert_eq!(start.status(), StatusCode::FOUND);
    let state = location_state(&start);
    let flow_cookie = set_cookie(&start, "oauth_flow").expect("flow cookie");
    let callback = send(
        &app,
        request(
            Method::GET,
            &format!("/oauth/{slug}/callback?state={state}&code=authorization-code"),
            &[(
                header::COOKIE.as_str(),
                format!("{}; {flow_cookie}", session_cookie(fixture.admin)),
            )],
            None,
        ),
    )
    .await;
    assert_eq!(callback.status(), StatusCode::FOUND);
    assert_eq!(
        callback.headers()[header::LOCATION],
        "/console/integrations"
    );
    let row = sqlx::query(
        "SELECT name, provider_email, dead FROM broker_credentials WHERE oauth_app_id = $1",
    )
    .bind(app_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("name"), "Slack – Grace Hopper");
    assert_eq!(
        row.get::<Option<String>, _>("provider_email").as_deref(),
        Some("grace@example.com")
    );
    assert!(!row.get::<bool, _>("dead"));
    let secret_name: String = sqlx::query_scalar(
        "SELECT name FROM static_secrets WHERE broker_credential_id = \
         (SELECT id FROM broker_credentials WHERE oauth_app_id = $1)",
    )
    .bind(app_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(secret_name, "Slack – Grace Hopper token");

    // A provider error leaves the credential connected and its generated names
    // untouched; it must not turn the consent into a failure page.
    let failing_slug = fixture.slug("slack-enrich-error");
    let failing_app_id = insert_app(
        &fixture.pool,
        &failing_slug,
        "slack",
        false,
        true,
        &["users:read.email".to_owned()],
        &serde_json::Map::new(),
    )
    .await;
    let start = send(
        &app,
        request(
            Method::GET,
            &format!("/oauth/{failing_slug}/start?scopes=users:read.email"),
            &admin_headers,
            None,
        ),
    )
    .await;
    assert_eq!(start.status(), StatusCode::FOUND);
    let state = location_state(&start);
    let flow_cookie = set_cookie(&start, "oauth_flow").expect("flow cookie");
    let callback = send(
        &app,
        request(
            Method::GET,
            &format!("/oauth/{failing_slug}/callback?state={state}&code=authorization-code"),
            &[(
                header::COOKIE.as_str(),
                format!("{}; {flow_cookie}", session_cookie(fixture.admin)),
            )],
            None,
        ),
    )
    .await;
    assert_eq!(callback.status(), StatusCode::FOUND);
    let row = sqlx::query(
        "SELECT name, provider_email, dead FROM broker_credentials WHERE oauth_app_id = $1",
    )
    .bind(failing_app_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("name"), "Slack – Other User");
    assert_eq!(row.get::<Option<String>, _>("provider_email"), None);
    assert!(!row.get::<bool, _>("dead"));
    let secret_name: String = sqlx::query_scalar(
        "SELECT name FROM static_secrets WHERE broker_credential_id = \
         (SELECT id FROM broker_credentials WHERE oauth_app_id = $1)",
    )
    .bind(failing_app_id)
    .fetch_one(&fixture.pool)
    .await
    .unwrap();
    assert_eq!(secret_name, "Slack – Other User token");

    handle.abort();
}

#[tokio::test]
async fn broker_lookup_and_show_hide_secrets() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let app = app(&fixture);
    let foreign_id = fixture.slug("lookup-broker");
    let headers = console_headers(fixture.admin);
    let created = send(
        &app,
        request(
            Method::POST,
            "/api/v1/console/broker_credentials",
            &headers,
            Some(json!({"data": {
                "foreign_id": foreign_id,
                "name": "Lookup",
                "token_endpoint": "https://auth.example.test/token",
                "client_id": "lookup-cid",
                "client_secret": "lookup-secret",
                "refresh_token": "lookup-refresh"
            }})),
        ),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);

    let lookup = send(
        &app,
        request(
            Method::GET,
            &format!("/api/v1/broker_credentials/lookup/{foreign_id}"),
            &bearer_headers(&fixture.api_token),
            None,
        ),
    )
    .await;
    assert_eq!(lookup.status(), StatusCode::OK);
    let body = json_body(lookup).await;
    assert_eq!(body["data"]["foreign_id"], foreign_id);
    assert!(!body.to_string().contains("lookup-secret"));
    assert!(!body.to_string().contains("lookup-refresh"));

    let missing = send(
        &app,
        request(
            Method::GET,
            "/api/v1/broker_credentials/lookup/missing-foreign-id",
            &bearer_headers(&fixture.api_token),
            None,
        ),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let message = json_body(missing).await["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(message, "BrokerCredential not found");
}

#[tokio::test]
async fn integrations_disconnect_refuses_another_users_credential() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let app = app(&fixture);
    let slug = fixture.slug("integration-owner");
    let app_id = fixture.app(&slug, "google", false, true).await;
    let credential_id = insert_credential(
        &fixture.pool,
        app_id,
        &fixture.slug("member-sub"),
        fixture.member,
        None,
    )
    .await;

    // Someone else's credential is a 404, not a 403, and stays live.
    let response = send(
        &app,
        request(
            Method::DELETE,
            &format!(
                "/api/v1/console/integrations/connections/{}",
                oid("bcr", credential_id)
            ),
            &console_headers(fixture.admin),
            None,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let dead: bool = sqlx::query_scalar("SELECT dead FROM broker_credentials WHERE id = $1")
        .bind(credential_id)
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert!(!dead, "a refused disconnect leaves the credential live");
}
