//! Identity on the simplified base: password sign-in, sign-out, the pending
//! page, launch, `/me`, and user administration.

use axum::http::Method;
use centaur_console_api::{
    auth::Access,
    identity,
    testing::{TEST_ORIGIN, TEST_PASSWORD, TestApp, TestClient, TestResponse},
};
use serde_json::json;

fn csrf_from_page(page: &str) -> String {
    let marker = "name=\"authenticity_token\" value=\"";
    let start = page.find(marker).expect("a CSRF field") + marker.len();
    page[start..start + page[start..].find('"').unwrap()].to_owned()
}

/// Loads `/login` and posts credentials, as a browser would.
async fn login_with(
    app: &TestApp,
    browser: &mut TestClient,
    email: &str,
    password: &str,
) -> TestResponse {
    let page = browser.send(app.get("/login").accept_html()).await;
    assert_eq!(page.status(), 200);
    let token = csrf_from_page(&page.text());
    browser
        .send(
            app.post("/login")
                .form(&[
                    ("email", email),
                    ("password", password),
                    ("authenticity_token", &token),
                ])
                .header("origin", TEST_ORIGIN),
        )
        .await
}

async fn me(app: &TestApp, browser: &mut TestClient) -> TestResponse {
    browser.send(app.get("/api/v1/console/me")).await
}

#[tokio::test]
async fn the_login_page_lists_providers_and_the_password_form() {
    let Some(app) = TestApp::with_env(
        identity::routes(),
        &[
            ("CENTAUR_CONSOLE_GOOGLE_CLIENT_ID", "id"),
            ("CENTAUR_CONSOLE_GOOGLE_CLIENT_SECRET", "secret"),
            ("CENTAUR_CONSOLE_SLACK_CLIENT_ID", "unfinished"),
        ],
    )
    .await
    else {
        return;
    };
    let page = app.get("/login").accept_html().send().await;
    assert_eq!(page.status(), 200);
    let html = page.text();
    assert!(html.contains("/auth/google/start") && !html.contains("/auth/slack/start"));
    assert!(
        html.contains("action=\"/login\"")
            && html.contains("name=\"email\"")
            && html.contains("name=\"password\"")
    );
    // The strict page policy: no script at all.
    assert!(
        page.header("content-security-policy")
            .unwrap()
            .contains("script-src 'none'")
    );
    assert_eq!(page.header("cache-control"), Some("no-store"));
    assert!(html.contains("/assets/oauth-page/oauth.css?v="));
}

#[tokio::test]
async fn password_sign_in_sets_a_session_and_one_shot_notice() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    let admin = app.create_user("admin@example.test", true, "active").await;
    let member = app
        .create_user("member@example.test", false, "active")
        .await;

    let mut browser = app.client();
    let response = login_with(&app, &mut browser, " Admin@Example.TEST ", TEST_PASSWORD).await;
    assert_eq!(
        (response.status(), response.location()),
        (302, Some("/console/principals"))
    );
    let cookie = response
        .set_cookies()
        .into_iter()
        .find(|c| c.starts_with("_centaur_console="))
        .unwrap();
    for attribute in ["HttpOnly", "SameSite=Lax", "Path=/"] {
        assert!(cookie.contains(attribute), "{cookie}");
    }
    let profile = me(&app, &mut browser).await;
    assert_eq!(profile.status(), 200);
    assert_eq!(profile.header("cache-control"), Some("no-store"));
    let body = profile.json();
    assert_eq!(body["data"]["user"]["email"], "admin@example.test");
    assert_eq!(
        body["data"]["user"]["id"],
        centaur_console_db::oid("usr", admin.id)
    );
    assert_eq!(body["data"]["auth_method"], "session");
    assert_eq!(
        body["data"]["flash"],
        json!({"notice": "Signed in as admin@example.test."})
    );
    assert!(body["data"]["csrf_token"].as_str().unwrap().len() >= 32);
    // The flash is handed off once.
    assert_eq!(
        me(&app, &mut browser).await.json()["data"]["flash"],
        json!({})
    );

    let mut browser = app.client();
    let response = login_with(&app, &mut browser, &member.email, TEST_PASSWORD).await;
    assert_eq!(response.location(), Some("/console/integrations"));
}

#[tokio::test]
async fn me_describes_the_caller_and_their_navigation() {
    let Some(app) = TestApp::with_env(
        identity::routes(),
        &[("CENTAUR_CONSOLE_API_CLIENTS_ENABLED", "true")],
    )
    .await
    else {
        return;
    };
    let admin = app.create_user("admin@example.test", true, "active").await;
    let member = app
        .create_user("member@example.test", false, "active")
        .await;
    let pending = app
        .create_user("pending@example.test", false, "pending")
        .await;
    let nav = |user: i64| {
        let (app, user) = (&app, user);
        async move {
            let mut browser = app.client();
            browser.sign_in(user);
            let body = me(app, &mut browser).await.json();
            // The token in /me is the one writes must send.
            assert_eq!(body["data"]["csrf_token"].as_str(), browser.csrf_token());
            body["data"].clone()
        }
    };
    let data = nav(admin.id).await;
    assert_eq!(data["acting_admin"], true);
    assert_eq!(
        data["features"],
        json!({"api_clients": true, "password_login": true})
    );
    assert_eq!(data["nav"]["users"], true);
    assert_eq!(data["nav"]["api_clients"], true);
    let data = nav(member.id).await;
    assert_eq!(data["acting_admin"], false);
    assert_eq!(data["nav"]["users"], false);
    assert_eq!(
        (
            data["nav"]["integrations"].clone(),
            data["nav"]["skills"].clone()
        ),
        (json!(true), json!(true))
    );
    let data = nav(pending.id).await;
    assert_eq!(data["user"]["status"], "pending");
    assert_eq!(data["nav"]["integrations"], false);
}

#[tokio::test]
async fn bad_credentials_are_indistinguishable_and_refused() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    app.create_user("member@example.test", false, "active")
        .await;
    app.create_user("disabled@example.test", false, "disabled")
        .await;

    let mut browser = app.client();
    for (email, password) in [
        ("member@example.test", "wrong"),
        ("nobody@example.test", TEST_PASSWORD),
        ("member@example.test", ""),
    ] {
        let response = login_with(&app, &mut browser, email, password).await;
        assert_eq!(response.status(), 422, "{email}");
        assert!(response.text().contains("Invalid email or password."));
        assert_eq!(me(&app, &mut browser).await.status(), 401);
    }
    let response = login_with(&app, &mut browser, "disabled@example.test", TEST_PASSWORD).await;
    assert_eq!(response.status(), 422);
    assert!(response.text().contains("Your account is disabled."));
    assert_eq!(me(&app, &mut browser).await.status(), 401);
}

#[tokio::test]
async fn a_pending_account_signs_in_to_the_holding_page() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    let pending = app
        .create_user("pending@example.test", false, "pending")
        .await;
    let mut browser = app.client();
    let response = login_with(&app, &mut browser, &pending.email, TEST_PASSWORD).await;
    assert_eq!(response.location(), Some("/pending"));
    let page = browser.send(app.get("/pending").accept_html()).await;
    assert_eq!(page.status(), 200);
    let html = page.text();
    assert!(
        html.contains("pending@example.test")
            && html.contains("Your account is awaiting approval.")
    );
    assert!(html.contains("action=\"/logout\""));
    // Pending users cannot use the console API, except /me and sign-out.
    assert_eq!(
        browser
            .send(app.get("/api/v1/console/users"))
            .await
            .error_reason()
            .as_deref(),
        Some("account_pending")
    );
    assert_eq!(me(&app, &mut browser).await.status(), 200);
    // Once approved, /pending sends them on.
    sqlx::query("UPDATE users SET status = 'active'")
        .execute(&app.db.pool)
        .await
        .unwrap();
    let response = browser.send(app.get("/pending").accept_html()).await;
    assert_eq!(response.location(), Some("/console/integrations"));
}

#[tokio::test]
async fn pages_require_a_session_and_a_csrf_token_for_posts() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    app.create_user("member@example.test", false, "active")
        .await;
    assert_eq!(app.get("/pending").send().await.location(), Some("/login"));
    assert_eq!(
        app.get("/launch?target=web%2Bcentaur://console")
            .send()
            .await
            .location(),
        Some("/login")
    );

    // No token, a forged token, and a foreign Origin all fail.
    let mut browser = app.client();
    let token = csrf_from_page(&browser.send(app.get("/login").accept_html()).await.text());
    let post = |token: Option<&str>| {
        let mut fields = vec![
            ("email", "member@example.test"),
            ("password", TEST_PASSWORD),
        ];
        fields.extend(token.map(|token| ("authenticity_token", token)));
        app.post("/login")
            .form(&fields)
            .header("origin", TEST_ORIGIN)
    };
    assert_eq!(browser.send(post(None)).await.status(), 403);
    assert_eq!(browser.send(post(Some("forged"))).await.status(), 403);
    let cross_site = post(Some(&token)).header("origin", "https://evil.example");
    assert_eq!(browser.send(cross_site).await.status(), 403);
    // No Origin at all (a non-browser client): refused even with the token.
    assert_eq!(
        browser
            .send(post(Some(&token)).without_origin())
            .await
            .status(),
        403
    );
    assert_eq!(me(&app, &mut browser).await.status(), 401);
    assert_eq!(browser.send(post(Some(&token))).await.status(), 302);
}

#[tokio::test]
async fn return_to_is_remembered_when_it_is_a_console_path_only() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    let member = app
        .create_user("member@example.test", false, "active")
        .await;
    for (return_to, expected) in [
        ("/console/roles?tab=1", "/console/roles?tab=1"),
        ("//evil.example/console", "/console/integrations"),
        ("https://evil.example", "/console/integrations"),
        ("/api/v1/console/me", "/console/integrations"),
        ("/console/../logout", "/console/integrations"),
    ] {
        let mut browser = app.client();
        let path = format!("/login?return_to={}", encode_query(return_to));
        browser.send(app.get(&path).accept_html()).await;
        let response = login_with(&app, &mut browser, &member.email, TEST_PASSWORD).await;
        assert_eq!(response.location(), Some(expected), "{return_to}");
    }
    // A signed-in user visiting /login is sent on, to an allowed return_to.
    let mut browser = app.client();
    browser.sign_in(member.id);
    let response = browser
        .send(app.get("/login?return_to=/console/skills"))
        .await;
    assert_eq!(response.location(), Some("/console/skills"));
    let response = browser
        .send(app.get("/login?return_to=//evil.example"))
        .await;
    assert_eq!(response.location(), Some("/console/integrations"));
}

fn encode_query(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[tokio::test]
async fn password_login_can_be_turned_off() {
    let Some(app) = TestApp::with_env(
        identity::routes(),
        &[("CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED", "false")],
    )
    .await
    else {
        return;
    };
    let member = app
        .create_user("member@example.test", false, "active")
        .await;
    let mut browser = app.client();
    let page = browser.send(app.get("/login").accept_html()).await;
    assert!(!page.text().contains("name=\"password\""));
    let token = browser.start_anonymous_session();
    let response = browser
        .send(
            app.post("/login")
                .form(&[
                    ("email", &member.email),
                    ("password", TEST_PASSWORD),
                    ("authenticity_token", &token),
                ])
                .header("origin", TEST_ORIGIN),
        )
        .await;
    assert_eq!(response.status(), 404);
    assert!(
        response
            .text()
            .contains("Email and password sign in is disabled.")
    );
    assert_eq!(me(&app, &mut browser).await.status(), 401);
    let features = {
        let mut other = app.client();
        other.sign_in(member.id);
        me(&app, &mut other).await.json()["data"]["features"].clone()
    };
    assert_eq!(features["password_login"], false);
}

#[tokio::test]
async fn sign_out_by_form_or_api_ends_the_session() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    let member = app
        .create_user("member@example.test", false, "active")
        .await;

    // The pending/console form: POST /logout with the page's token.
    let mut browser = app.client();
    login_with(&app, &mut browser, &member.email, TEST_PASSWORD).await;
    let profile = me(&app, &mut browser).await.json();
    let token = profile["data"]["csrf_token"].as_str().unwrap().to_owned();
    assert_eq!(
        browser
            .send(app.post("/logout").form(&[]).header("origin", TEST_ORIGIN))
            .await
            .status(),
        403,
        "token required"
    );
    // The token alone is not enough without a same-origin Origin.
    assert_eq!(
        browser
            .send(app.post("/logout").form(&[("authenticity_token", &token)]))
            .await
            .status(),
        403,
        "Origin required"
    );
    let response = browser
        .send(
            app.post("/logout")
                .form(&[("authenticity_token", &token)])
                .header("origin", TEST_ORIGIN),
        )
        .await;
    assert_eq!(
        (response.status(), response.location()),
        (302, Some("/login"))
    );
    assert_eq!(me(&app, &mut browser).await.status(), 401);
    let page = browser.send(app.get("/login").accept_html()).await;
    assert!(page.text().contains("Signed out."));

    // DELETE /logout with the header token, and the SPA's DELETE /session.
    let mut browser = app.client();
    browser.sign_in(member.id);
    let response = browser
        .send(app.request(Method::DELETE, "/logout").csrf(&browser))
        .await;
    assert_eq!(response.location(), Some("/login"));
    assert_eq!(me(&app, &mut browser).await.status(), 401);

    let mut browser = app.client();
    browser.sign_in(member.id);
    let response = browser
        .send(app.delete("/api/v1/console/session").csrf(&browser))
        .await;
    assert_eq!(response.status(), 204);
    assert_eq!(me(&app, &mut browser).await.status(), 401);
}

#[tokio::test]
async fn launch_opens_console_paths_for_active_users() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    let member = app
        .create_user("member@example.test", false, "active")
        .await;
    let pending = app
        .create_user("pending@example.test", false, "pending")
        .await;
    let mut browser = app.client();
    browser.sign_in(member.id);
    for (target, expected) in [
        ("web%2Bcentaur://console/workflows", "/console/workflows"),
        ("web%2Bcentaur://evil.example/console", "/"),
        ("https://evil.example", "/"),
        ("", "/"),
    ] {
        let response = browser
            .send(app.get(&format!("/launch?target={target}")))
            .await;
        assert_eq!(response.location(), Some(expected), "{target}");
    }
    let mut browser = app.client();
    browser.sign_in(pending.id);
    assert_eq!(
        browser.send(app.get("/launch?target=x")).await.location(),
        Some("/pending")
    );
}

#[tokio::test]
async fn the_pwa_files_are_served_without_a_session() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    let manifest = app.get("/manifest").send().await;
    assert_eq!(manifest.status(), 200);
    assert_eq!(
        manifest.header("content-type"),
        Some("application/manifest+json")
    );
    assert!(manifest.json()["name"].is_string());
    let worker = app.get("/service-worker").send().await;
    assert_eq!(worker.header("content-type"), Some("text/javascript"));
    assert!(manifest.set_cookies().is_empty() && worker.set_cookies().is_empty());
}

// --- users --------------------------------------------------------------------

#[tokio::test]
async fn user_endpoints_authorization_matrix() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    let target = app
        .create_user("target@example.test", false, "pending")
        .await;
    let id = centaur_console_db::oid("usr", target.id);
    app.assert_console_access(Access::Pending, Method::GET, "/api/v1/console/me", None)
        .await;
    app.assert_console_access(
        Access::Pending,
        Method::DELETE,
        "/api/v1/console/session",
        None,
    )
    .await;
    app.assert_console_access(Access::Admin, Method::GET, "/api/v1/console/users", None)
        .await;
    for action in ["approve", "disable", "promote", "demote"] {
        let path = format!("/api/v1/console/users/{id}/{action}");
        app.assert_console_access(Access::Admin, Method::POST, &path, Some(&json!({})))
            .await;
    }
}

async fn admin_browser(app: &TestApp) -> (TestClient, i64) {
    let admin = app.create_user("root@example.test", true, "active").await;
    let mut browser = app.client();
    browser.sign_in(admin.id);
    (browser, admin.id)
}

#[tokio::test]
async fn listing_users_filters_by_status_and_marks_the_caller() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    let (mut browser, admin_id) = admin_browser(&app).await;
    let member = app
        .create_user("member@example.test", false, "active")
        .await;
    app.create_user("pending@example.test", false, "pending")
        .await;
    sqlx::query("INSERT INTO user_identities (user_id, provider, subject, created_at, updated_at) VALUES ($1, 'slack', 's', now(), now()), ($1, 'google', 'g', now(), now()), ($1, 'slack', 's2', now(), now())")
        .bind(member.id)
        .execute(&app.db.pool)
        .await
        .unwrap();

    let response = browser.send(app.get("/api/v1/console/users")).await;
    assert_eq!(response.status(), 200);
    let body = response.json();
    assert_eq!(body["meta"]["total"], 3);
    let emails: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["email"].as_str().unwrap())
        .collect();
    assert_eq!(
        emails,
        [
            "root@example.test",
            "member@example.test",
            "pending@example.test"
        ],
        "admins first, then by email"
    );
    let root = &body["data"][0];
    assert_eq!(root["current"], true);
    assert_eq!(root["id"], centaur_console_db::oid("usr", admin_id));
    assert_eq!(
        body["data"][1]["identity_providers"],
        json!(["google", "slack"])
    );
    assert!(root["created_at"].as_str().unwrap().ends_with('Z'));

    let pending = browser
        .send(app.get("/api/v1/console/users?status=pending"))
        .await
        .json();
    assert_eq!(pending["meta"]["total"], 1);
    let response = browser
        .send(app.get("/api/v1/console/users?status=bogus"))
        .await;
    assert_eq!(response.status(), 400);
    assert_eq!(
        response.error_message().as_deref(),
        Some("status must be one of pending, active, disabled")
    );
}

async fn post(app: &TestApp, browser: &mut TestClient, path: &str) -> TestResponse {
    browser
        .send(app.post(path).json(&json!({})).csrf(browser))
        .await
}

#[tokio::test]
async fn approving_promoting_and_demoting_users() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    let (mut browser, admin_id) = admin_browser(&app).await;
    let user = app.create_user("new@example.test", false, "pending").await;
    let base = format!(
        "/api/v1/console/users/{}",
        centaur_console_db::oid("usr", user.id)
    );

    let body = post(&app, &mut browser, &format!("{base}/approve"))
        .await
        .json();
    assert_eq!(body["data"]["status"], "active");
    assert_eq!(
        body["data"]["approved_by"],
        json!({"id": centaur_console_db::oid("usr", admin_id), "email": "root@example.test"})
    );
    assert!(body["data"]["approved_at"].is_string());

    let body = post(&app, &mut browser, &format!("{base}/promote"))
        .await
        .json();
    assert_eq!(
        (
            body["data"]["admin"].clone(),
            body["data"]["status"].clone()
        ),
        (json!(true), json!("active"))
    );
    let body = post(&app, &mut browser, &format!("{base}/demote"))
        .await
        .json();
    assert_eq!(body["data"]["admin"], false);

    // Promoting a disabled user also re-enables them.
    sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
        .bind(user.id)
        .execute(&app.db.pool)
        .await
        .unwrap();
    let body = post(&app, &mut browser, &format!("{base}/promote"))
        .await
        .json();
    assert_eq!(
        (
            body["data"]["admin"].clone(),
            body["data"]["status"].clone()
        ),
        (json!(true), json!("active"))
    );
}

#[tokio::test]
async fn disabling_a_user_revokes_their_mcp_refresh_tokens() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    let (mut browser, _) = admin_browser(&app).await;
    let user = app
        .create_user("victim@example.test", false, "active")
        .await;
    let principal = app.create_principal("slack_channel", "ops", user.id).await;
    let client: i64 = sqlx::query_scalar(
        "INSERT INTO mcp_oauth_clients (created_at, updated_at) VALUES (now(), now()) RETURNING id",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mcp_oauth_refresh_tokens (mcp_oauth_client_id, principal_id, user_id, resource, token_hash, expires_at, created_at, updated_at) \
         VALUES ($1, $2, $3, 'r', 'hash', $4, now(), now())",
    )
    .bind(client)
    .bind(principal)
    .bind(user.id)
    .bind((app.now() + chrono::Duration::days(1)).naive_utc())
    .execute(&app.db.pool)
    .await
    .unwrap();

    let path = format!(
        "/api/v1/console/users/{}/disable",
        centaur_console_db::oid("usr", user.id)
    );
    let body = post(&app, &mut browser, &path).await.json();
    assert_eq!(body["data"]["status"], "disabled");
    let revoked: Option<chrono::NaiveDateTime> =
        sqlx::query_scalar("SELECT revoked_at FROM mcp_oauth_refresh_tokens")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(revoked, Some(app.now().naive_utc()));
}

#[tokio::test]
async fn admins_cannot_disable_or_demote_themselves_and_unknown_users_are_404() {
    let Some(app) = TestApp::new(identity::routes()).await else {
        return;
    };
    let (mut browser, admin_id) = admin_browser(&app).await;
    let own = format!(
        "/api/v1/console/users/{}",
        centaur_console_db::oid("usr", admin_id)
    );
    for (action, message) in [
        ("disable", "You can't disable your own account."),
        ("demote", "You can't demote your own account."),
    ] {
        let response = post(&app, &mut browser, &format!("{own}/{action}")).await;
        assert_eq!(response.status(), 422);
        assert_eq!(response.error_message().as_deref(), Some(message));
        assert_eq!(response.error_reason().as_deref(), Some("self"));
    }
    let row: (String, bool) = sqlx::query_as("SELECT status, admin FROM users WHERE id = $1")
        .bind(admin_id)
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(row, ("active".to_owned(), true));
    for id in [
        "usr_unknown",
        "nope",
        &centaur_console_db::oid("prn", 1),
        &centaur_console_db::oid("usr", 999_999),
    ] {
        let response = post(
            &app,
            &mut browser,
            &format!("/api/v1/console/users/{id}/approve"),
        )
        .await;
        assert_eq!(response.status(), 404, "{id}");
    }
}
