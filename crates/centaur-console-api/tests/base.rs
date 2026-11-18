//! The simplified HTTP base: namespaces and their guards, extractors, the
//! error envelope, static files, and the fallbacks. Uses throwaway routes.

use axum::{
    Router,
    routing::{get, post},
};
use centaur_console_api::{
    ApiError, ApiResult, AppState, Routes,
    auth::{Access, BearerCaller, ConsoleCaller, InternalCaller, SandboxCaller},
    extract::{Data, Json, Path, Query, data},
    testing::TestApp,
};
use http::Method;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Widget {
    name: String,
}

#[derive(Deserialize)]
struct Paging {
    page: Option<u32>,
}

async fn echo(Data(widget): Data<Widget>) -> ApiResult<Json<Value>> {
    if widget.name.is_empty() {
        return Err(ApiError::field("name", "can't be blank"));
    }
    Ok(data(json!({"name": widget.name})))
}

async fn raw(Json(value): Json<Value>) -> Json<Value> {
    Json(value)
}

async fn paged(Query(paging): Query<Paging>, Path(id): Path<u32>) -> Json<Value> {
    Json(json!({"page": paging.page, "id": id}))
}

async fn who_console(ConsoleCaller(user): ConsoleCaller) -> Json<Value> {
    Json(json!({"email": user.email}))
}

async fn who_bearer(BearerCaller(auth): BearerCaller) -> Json<Value> {
    Json(json!({"email": auth.user.email}))
}

async fn who_sandbox(SandboxCaller(auth): SandboxCaller) -> Json<Value> {
    Json(json!({"proxy": auth.proxy_name}))
}

async fn internal(_: InternalCaller) -> &'static str {
    "ok"
}

async fn boom() -> &'static str {
    panic!("handler bug")
}

async fn conflict() -> ApiResult<&'static str> {
    Err(ApiError::Conflict("already running".into()))
}

fn routes() -> Routes {
    Routes {
        bearer: Router::<AppState>::new()
            .route("/widgets", post(echo).get(who_bearer))
            .route("/raw", post(raw))
            .route("/widgets/{id}", get(paged))
            .route("/boom", get(boom))
            // Wrong credential for the namespace: must refuse, not run.
            .route("/console-only", get(who_console)),
        console: Router::new().route("/widgets", get(who_console).post(who_console)),
        console_active: Router::new().route("/mine", get(who_console).post(who_console)),
        console_pending: Router::new().route("/status", get(who_console)),
        sandbox: Router::new().route("/me", get(who_sandbox)),
        internal: Router::new()
            .route("/v1/ping", get(internal))
            .route("/v1/conflict", post(conflict)),
        ..Routes::default()
    }
}

#[tokio::test]
async fn data_wrapper_validation_and_json_errors() {
    let Some(app) = TestApp::new(routes()).await else {
        return;
    };
    let admin = app.create_user("admin@example.test", true, "active").await;
    let key = app.create_api_key(admin.id).await;
    let post = |body: Value| app.post("/api/v1/widgets").bearer(&key).json(&body);

    let response = post(json!({"data": {"name": "gear"}})).send().await;
    assert_eq!(
        (response.status(), response.json()),
        (200, json!({"data": {"name": "gear"}}))
    );

    let response = post(json!({"data": {"name": ""}})).send().await;
    assert_eq!(response.status(), 422);
    assert_eq!(response.json()["error"]["message"], "validation failed");
    assert_eq!(
        response.json()["error"]["details"]["name"],
        json!(["can't be blank"])
    );

    // Missing wrapper, wrong field types, and malformed JSON are 400s in the envelope.
    for body in [
        json!({"name": "gear"}),
        json!({"data": {"name": 3}}),
        json!({"data": null}),
    ] {
        let response = post(body).send().await;
        assert_eq!(response.status(), 400, "{}", response.text());
        assert!(response.json()["error"]["message"].is_string());
    }
    let response = app
        .post("/api/v1/raw")
        .bearer(&key)
        .raw_body("application/json", b"{nope")
        .send()
        .await;
    assert_eq!(response.status(), 400);
    // No content type: not JSON.
    let response = app
        .post("/api/v1/raw")
        .bearer(&key)
        .raw_body("text/plain", b"{}")
        .send()
        .await;
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn path_and_query_extractors() {
    let Some(app) = TestApp::new(routes()).await else {
        return;
    };
    let admin = app.create_user("admin@example.test", true, "active").await;
    let key = app.create_api_key(admin.id).await;
    let response = app
        .get("/api/v1/widgets/7?page=2")
        .bearer(&key)
        .send()
        .await;
    assert_eq!(response.json(), json!({"page": 2, "id": 7}));
    assert_eq!(
        app.get("/api/v1/widgets/nope")
            .bearer(&key)
            .send()
            .await
            .status(),
        404
    );
    assert_eq!(
        app.get("/api/v1/widgets/7?page=x")
            .bearer(&key)
            .send()
            .await
            .status(),
        400
    );
}

#[tokio::test]
async fn unknown_paths_are_json_404s_and_wrong_methods_json_405s() {
    let Some(app) = TestApp::new(routes()).await else {
        return;
    };
    for path in [
        "/api/v1/nothing",
        "/api/v1/console/nothing",
        "/internal/v1/nothing",
        "/api/other",
    ] {
        for accept in ["application/json", "text/html"] {
            let response = app.get(path).header("accept", accept).send().await;
            assert_eq!(response.status(), 404, "{path} {accept}");
            assert_eq!(response.error_message().as_deref(), Some("not found"));
        }
    }
    let response = app.request(Method::POST, "/up").send().await;
    assert_eq!(response.status(), 405);
    assert!(response.error_message().is_some());
    let response = app.request(Method::DELETE, "/api/v1/raw").send().await;
    assert_eq!(response.status(), 405);
}

#[tokio::test]
async fn health_spa_fallback_and_security_headers() {
    let Some(app) = TestApp::new(routes()).await else {
        return;
    };
    let up = app.get("/up").send().await;
    assert_eq!(up.status(), 200);
    for (name, value) in [
        ("x-frame-options", "DENY"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "strict-origin-when-cross-origin"),
    ] {
        assert_eq!(up.header(name), Some(value), "{name}");
    }
    assert!(
        up.header("content-security-policy")
            .unwrap()
            .contains("frame-ancestors 'none'")
    );

    // A browser navigation gets the SPA shell; an API client gets JSON.
    let shell = app.get("/console/principals").accept_html().send().await;
    assert_eq!(shell.status(), 200);
    assert!(shell.text().contains("id=\"root\""));
    assert_eq!(shell.header("cache-control"), Some("no-cache"));
    assert_eq!(app.get("/console/principals").send().await.status(), 404);
    // Protocol and API prefixes never get the shell, even for browsers.
    for path in [
        "/api/v1/nope",
        "/mcp/nope",
        "/oauth/x/nope",
        "/.well-known/nope",
    ] {
        assert_eq!(
            app.get(path).accept_html().send().await.status(),
            404,
            "{path}"
        );
    }
    // Built assets are served from public/.
    let css = app.get("/assets/oauth-page/oauth.css").send().await;
    assert_eq!(css.status(), 200);
    assert!(css.header("content-type").unwrap().starts_with("text/css"));
}

#[tokio::test]
async fn panics_become_json_500s_without_details() {
    let Some(app) = TestApp::new(routes()).await else {
        return;
    };
    let admin = app.create_user("admin@example.test", true, "active").await;
    let key = app.create_api_key(admin.id).await;
    let response = app.get("/api/v1/boom").bearer(&key).send().await;
    assert_eq!(response.status(), 500);
    assert!(!response.text().contains("handler bug"));
}

#[tokio::test]
async fn bearer_namespace_authorization_matrix() {
    let Some(app) = TestApp::new(routes()).await else {
        return;
    };
    app.assert_bearer_access(Method::GET, "/api/v1/widgets", None)
        .await;
    app.assert_bearer_access(
        Method::POST,
        "/api/v1/widgets",
        Some(&json!({"data": {"name": "x"}})),
    )
    .await;
}

#[tokio::test]
async fn console_namespace_authorization_matrix() {
    let Some(app) = TestApp::new(routes()).await else {
        return;
    };
    let body = json!({});
    app.assert_console_access(Access::Admin, Method::GET, "/api/v1/console/widgets", None)
        .await;
    app.assert_console_access(
        Access::Admin,
        Method::POST,
        "/api/v1/console/widgets",
        Some(&body),
    )
    .await;
    app.assert_console_access(Access::Active, Method::GET, "/api/v1/console/mine", None)
        .await;
    app.assert_console_access(
        Access::Active,
        Method::POST,
        "/api/v1/console/mine",
        Some(&body),
    )
    .await;
    app.assert_console_access(Access::Pending, Method::GET, "/api/v1/console/status", None)
        .await;
}

#[tokio::test]
async fn a_disabled_session_is_signed_out_and_a_bearer_key_never_opens_the_console() {
    let Some(app) = TestApp::new(routes()).await else {
        return;
    };
    let admin = app.create_user("admin@example.test", true, "active").await;
    let key = app.create_api_key(admin.id).await;
    let response = app.get("/api/v1/console/widgets").bearer(&key).send().await;
    assert_eq!(
        (response.status(), response.error_reason().as_deref()),
        (403, Some("session_required"))
    );

    let mut browser = app.client();
    browser.sign_in(admin.id);
    assert_eq!(
        browser
            .send(app.get("/api/v1/console/widgets"))
            .await
            .status(),
        200
    );
    sqlx::query("UPDATE users SET status = 'disabled'")
        .execute(&app.db.pool)
        .await
        .unwrap();
    let response = browser.send(app.get("/api/v1/console/widgets")).await;
    assert_eq!(response.error_reason().as_deref(), Some("account_disabled"));
    // The session was cleared: the next request is simply anonymous.
    sqlx::query("UPDATE users SET status = 'active'")
        .execute(&app.db.pool)
        .await
        .unwrap();
    let response = browser.send(app.get("/api/v1/console/widgets")).await;
    assert_eq!(response.error_reason().as_deref(), Some("session_required"));
}

#[tokio::test]
async fn credentials_do_not_cross_namespaces() {
    let Some(app) = TestApp::new(routes()).await else {
        return;
    };
    let admin = app.create_user("admin@example.test", true, "active").await;
    let key = app.create_api_key(admin.id).await;
    // A console handler mounted under the bearer namespace finds no console
    // credential and refuses to run.
    let response = app.get("/api/v1/console-only").bearer(&key).send().await;
    assert_eq!(response.status(), 401);
    assert_eq!(
        response.error_reason().as_deref(),
        Some("credential_namespace_mismatch")
    );
    // A sandbox token is not an API key, and an API key is not a sandbox token.
    let principal = app.create_principal("slack_channel", "ops", admin.id).await;
    let proxy = app.create_proxy("sbx-1", principal).await;
    let token = app.sandbox_token(proxy, principal).await;
    assert_eq!(
        app.get("/api/v1/widgets")
            .bearer(&token)
            .send()
            .await
            .status(),
        401
    );
    assert_eq!(
        app.get("/api/v1/sandbox/me")
            .bearer(&key)
            .send()
            .await
            .status(),
        401
    );
    let response = app.get("/api/v1/sandbox/me").bearer(&token).send().await;
    assert_eq!(response.json(), json!({"proxy": "sbx-1"}));
    // The internal service token opens only /internal.
    let service = app.internal_token();
    assert_eq!(
        app.get("/internal/v1/ping")
            .bearer(&service)
            .send()
            .await
            .status(),
        200
    );
    assert_eq!(
        app.get("/api/v1/widgets")
            .bearer(&service)
            .send()
            .await
            .status(),
        401
    );
    assert_eq!(
        app.get("/api/v1/sandbox/me")
            .bearer(&service)
            .send()
            .await
            .status(),
        401
    );
}

#[tokio::test]
async fn internal_errors_are_flat_and_other_errors_use_the_envelope() {
    let Some(app) = TestApp::new(routes()).await else {
        return;
    };
    for request in [
        app.get("/internal/v1/ping"),
        app.get("/internal/v1/ping").bearer("junk"),
    ] {
        let response = request.send().await;
        assert_eq!(response.status(), 401);
        assert_eq!(
            response.json(),
            json!({"error": "invalid or missing service token"})
        );
    }
    let service = app.internal_token();
    let response = app.get("/internal/v1/ping").bearer(&service).send().await;
    assert_eq!(response.status(), 200);
    let response = app
        .post("/internal/v1/conflict")
        .bearer(&app.internal_token())
        .send()
        .await;
    assert_eq!(response.status(), 409);
    assert_eq!(
        response.json(),
        json!({"error": {"message": "already running"}})
    );
}

#[tokio::test]
async fn sandbox_tokens_are_bound_to_their_proxy_and_principal() {
    let Some(app) = TestApp::new(routes()).await else {
        return;
    };
    let admin = app.create_user("admin@example.test", true, "active").await;
    let one = app.create_principal("slack_channel", "one", admin.id).await;
    let two = app.create_principal("slack_channel", "two", admin.id).await;
    let proxy = app.create_proxy("sbx-1", one).await;
    let good = app.sandbox_token(proxy, one).await;
    let wrong_principal = app.sandbox_token(proxy, two).await;
    assert_eq!(
        app.get("/api/v1/sandbox/me")
            .bearer(&good)
            .send()
            .await
            .status(),
        200
    );
    assert_eq!(
        app.get("/api/v1/sandbox/me")
            .bearer(&wrong_principal)
            .send()
            .await
            .status(),
        401
    );
    assert_eq!(app.get("/api/v1/sandbox/me").send().await.status(), 401);
    // Reassigning the proxy invalidates the token immediately.
    sqlx::query("UPDATE proxies SET principal_id = $1")
        .bind(two)
        .execute(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(
        app.get("/api/v1/sandbox/me")
            .bearer(&good)
            .send()
            .await
            .status(),
        401
    );
}

#[tokio::test]
async fn unknown_hosts_are_refused_except_for_probes() {
    let env = [
        ("CENTAUR_CONSOLE_PUBLIC_URL", "https://Console.Example.com"),
        ("CENTAUR_CONSOLE_URL", "http://console:3000"),
        (
            "CENTAUR_CONSOLE_ALLOWED_HOSTS",
            "extra.internal, other.internal:8080",
        ),
    ];
    let Some(app) = TestApp::build(Routes::default(), &env, |_| {}).await else {
        return;
    };
    for host in [
        "console.example.com",
        "CONSOLE.example.com:443",
        "console:3000",
        "extra.internal",
        "other.internal",
    ] {
        let response = app
            .get("/api/v1/console/nothing")
            .header("host", host)
            .send()
            .await;
        assert_ne!(response.status(), 403, "{host}");
    }
    for host in [
        "evil.example",
        "console.example.com.evil.example",
        "10.1.2.3",
    ] {
        let response = app
            .get("/api/v1/console/nothing")
            .header("host", host)
            .send()
            .await;
        assert_eq!(response.status(), 403, "{host}");
        assert_eq!(
            response.json()["error"]["message"],
            "host not allowed",
            "{host}"
        );
    }
    let probe = app.get("/up").header("host", "10.1.2.3").send().await;
    assert_eq!(probe.status(), 200);
}
