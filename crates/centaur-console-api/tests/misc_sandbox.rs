//! What a sandbox may read about itself: permissions and OAuth start URLs.

mod misc_common;

use axum::http::Method;
use centaur_console_api::{misc, testing::TestApp};
use centaur_console_db::oid;
use misc_common::{sandbox, sandbox_call};
use serde_json::json;

const SECRET: &str = "PLAINTEXT-SENTINEL-4242";

async fn exec(app: &TestApp, sql: &str) {
    sqlx::query(sql).execute(&app.db.pool).await.unwrap();
}

#[tokio::test]
async fn sandbox_routes_refuse_missing_and_foreign_credentials() {
    let Some(app) = TestApp::new(misc::routes()).await else {
        return;
    };
    let admin = app
        .create_user("admin@example.test", true, "active")
        .await
        .id;
    for path in ["/api/v1/sandbox/permissions", "/api/v1/sandbox/oauth_apps"] {
        assert_eq!(app.get(path).send().await.status(), 401, "{path}");
        assert_eq!(
            app.get(path).bearer("forged").send().await.status(),
            401,
            "{path}"
        );
        let key = app.create_api_key(admin).await;
        assert_eq!(
            app.get(path).bearer(&key).send().await.status(),
            401,
            "{path}"
        );
    }
    // Permissions bind the token to its proxy and principal; the OAuth start
    // list only verifies the signature.
    let principal = app.create_principal("unknown", "p", admin).await;
    let proxy = app.create_proxy("sandbox-a", principal).await;
    let token = app.sandbox_token(proxy, principal).await;
    sqlx::query("UPDATE proxies SET principal_id = NULL WHERE id = $1")
        .bind(proxy)
        .execute(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(
        sandbox_call(
            &app,
            &token,
            Method::GET,
            "/api/v1/sandbox/permissions",
            None
        )
        .await
        .status(),
        401
    );
    assert_eq!(
        sandbox_call(
            &app,
            &token,
            Method::GET,
            "/api/v1/sandbox/oauth_apps",
            None
        )
        .await
        .status(),
        200
    );
}

#[tokio::test]
async fn permissions_merge_direct_and_role_grants_and_redact_secrets() {
    let Some(app) = TestApp::new(misc::routes()).await else {
        return;
    };
    let admin = app
        .create_user("admin@example.test", true, "active")
        .await
        .id;
    let principal = app.create_principal("slack_dm", "Dana", admin).await;
    exec(&app, &format!(
        "UPDATE principals SET sandbox_repo_cache = 'none', sandbox_workflows_read_enabled = TRUE, \
         foreign_id = 'ext-1' WHERE id = {principal}")).await;
    let now = app.now().naive_utc();
    let role: i64 = sqlx::query_scalar("INSERT INTO roles (name, created_by_id, created_at, updated_at) VALUES ('ops', $1, $2, $2) RETURNING id")
        .bind(admin).bind(now).fetch_one(&app.db.pool).await.unwrap();
    exec(&app, &format!("INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) VALUES ({principal}, {role}, now(), now())")).await;
    // Slack channel permissions: a direct and a role row for the same channel merge.
    exec(&app, &format!(
        "INSERT INTO slack_channel_permissions (channel_id, principal_id, upload_enabled, created_at, updated_at) \
         VALUES (' c123 ', {principal}, TRUE, now(), now())")).await;
    exec(&app, &format!(
        "INSERT INTO slack_channel_permissions (channel_id, role_id, history_enabled, created_at, updated_at) \
         VALUES ('C123', {role}, TRUE, now(), now()), ('C999', {role}, FALSE, now(), now())")).await;
    // A granted static secret with an inline value and a tool label.
    let secret: i64 = sqlx::query_scalar(
        "INSERT INTO static_secrets (name, kind, labels, inject_config, created_by_id, created_at, updated_at) \
         VALUES ('gh', 'custom', '{\"centaur-tool\": \" github \"}', '{\"header\":\"Authorization\",\"formatter\":\"Bearer {{ .Value }}\"}', $1, $2, $2) RETURNING id")
        .bind(admin).bind(now).fetch_one(&app.db.pool).await.unwrap();
    sqlx::query("INSERT INTO secret_sources (source_type, secret, config, static_secret_id, created_at, updated_at) VALUES ('control_plane', $1, '{}', $2, $3, $3)")
        .bind(app.state.encryptor.encrypt(SECRET).unwrap()).bind(secret).bind(now).execute(&app.db.pool).await.unwrap();
    exec(&app, &format!("INSERT INTO request_rules (static_secret_id, host, created_at, updated_at) VALUES ({secret}, 'api.example.test', now(), now())")).await;
    exec(&app, &format!("INSERT INTO grants (role_id, static_secret_id, priority, created_by_id, created_at, updated_at) VALUES ({role}, {secret}, 100, {admin}, now(), now())")).await;
    // An OAuth broker credential behind another granted secret.
    let oauth_app: i64 = sqlx::query_scalar(
        "INSERT INTO oauth_apps (slug, provider, client_id, created_by_id, created_at, updated_at) \
         VALUES ('google', 'google', 'cid', $1, $2, $2) RETURNING id",
    )
    .bind(admin)
    .bind(now)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    let credential: i64 = sqlx::query_scalar(
        "INSERT INTO broker_credentials (oauth_app_id, token_endpoint, provider_email, scopes, created_at, updated_at) \
         VALUES ($1, 'https://idp.example.test/token', 'dana@example.test', '[\"mail\"]', $2, $2) RETURNING id")
        .bind(oauth_app).bind(now).fetch_one(&app.db.pool).await.unwrap();
    let broker_secret: i64 = sqlx::query_scalar(
        "INSERT INTO static_secrets (name, kind, broker_credential_id, created_by_id, created_at, updated_at) \
         VALUES ('oauth', 'custom', $1, $2, $3, $3) RETURNING id")
        .bind(credential).bind(admin).bind(now).fetch_one(&app.db.pool).await.unwrap();
    exec(&app, &format!("INSERT INTO grants (principal_id, static_secret_id, priority, created_by_id, created_at, updated_at) VALUES ({principal}, {broker_secret}, 100, {admin}, now(), now())")).await;

    let proxy = app.create_proxy("sandbox-dana", principal).await;
    let token = app.sandbox_token(proxy, principal).await;
    let response = sandbox_call(
        &app,
        &token,
        Method::GET,
        "/api/v1/sandbox/permissions",
        None,
    )
    .await;
    assert_eq!(response.status(), 200, "{}", response.text());
    assert_eq!(response.header("cache-control"), Some("no-store"));
    let etag = response.header("etag").unwrap().to_owned();
    assert!(etag.starts_with('"') && etag.len() > 10);
    let text = response.text();
    assert!(!text.contains(SECRET), "inline secrets are redacted");
    assert!(text.contains("[redacted]"));
    let data = &response.json()["data"];
    assert_eq!(data["sandbox_id"], "sandbox-dana");
    assert_eq!(data["proxy_id"], oid("prx", proxy));
    assert_eq!(data["principal_id"], oid("prn", principal));
    assert_eq!(
        data["principal"],
        json!({"id": oid("prn", principal), "foreign_id": "ext-1", "name": "Dana"})
    );
    assert_eq!(
        data["capabilities"],
        json!({
            "sandbox_repo_cache": "none",
            "sandbox_observability_enabled": true,
            "sandbox_sessions_read_enabled": false,
            "sandbox_workflows_read_enabled": true,
            "sandbox_workflows_write_enabled": false,
        })
    );
    assert_eq!(
        data["slack_channel_permissions"],
        json!([
            {"channel_id": "C123", "upload_enabled": true, "download_enabled": false, "history_enabled": true},
            {"channel_id": "C999", "upload_enabled": false, "download_enabled": false, "history_enabled": false},
        ])
    );
    assert_eq!(data["connected_tools"], json!(["github"]));
    assert_eq!(
        data["oauth_credentials"],
        json!([{
            "id": oid("bcr", credential),
            "oauth_app_id": oid("oap", oauth_app),
            "slug": "google",
            "provider": "google",
            "provider_email": "dana@example.test",
            "provider_subject": null,
            "status": "bootstrapping",
            "scopes": ["mail"],
        }])
    );
    assert!(
        !data["permissions"]["secrets"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        data["permissions"]["transforms"].is_array() && data["permissions"]["postgres"].is_array()
    );
    // The same state hashes to the same ETag.
    let again = sandbox_call(
        &app,
        &token,
        Method::GET,
        "/api/v1/sandbox/permissions",
        None,
    )
    .await;
    assert_eq!(again.header("etag"), Some(etag.as_str()));
    exec(
        &app,
        &format!("UPDATE broker_credentials SET dead = TRUE WHERE id = {credential}"),
    )
    .await;
    let dead = sandbox_call(
        &app,
        &token,
        Method::GET,
        "/api/v1/sandbox/permissions",
        None,
    )
    .await;
    assert_eq!(
        dead.json()["data"]["oauth_credentials"][0]["status"],
        "dead"
    );
    assert_ne!(dead.header("etag"), Some(etag.as_str()));
}

#[tokio::test]
async fn another_principals_grants_are_not_visible() {
    let Some(app) = TestApp::new(misc::routes()).await else {
        return;
    };
    let admin = app
        .create_user("admin@example.test", true, "active")
        .await
        .id;
    let mine = sandbox(&app, "mine", None, admin).await;
    let other = app.create_principal("unknown", "other", admin).await;
    exec(&app, &format!(
        "INSERT INTO slack_channel_permissions (channel_id, principal_id, upload_enabled, created_at, updated_at) \
         VALUES ('C777', {other}, TRUE, now(), now())")).await;
    let response = sandbox_call(
        &app,
        &mine,
        Method::GET,
        "/api/v1/sandbox/permissions",
        None,
    )
    .await;
    assert_eq!(
        response.json()["data"]["slack_channel_permissions"],
        json!([])
    );
    assert_eq!(response.json()["data"]["connected_tools"], json!([]));
    assert_eq!(response.json()["data"]["oauth_credentials"], json!([]));
}

#[tokio::test]
async fn oauth_apps_list_enabled_non_shared_apps_with_start_urls() {
    let Some(app) = TestApp::with_env(
        misc::routes(),
        &[(
            "CENTAUR_CONSOLE_PUBLIC_URL",
            "https://console.example.test/",
        )],
    )
    .await
    else {
        return;
    };
    let admin = app
        .create_user("admin@example.test", true, "active")
        .await
        .id;
    let token = sandbox(&app, "any", None, admin).await;
    for (slug, enabled, shared) in [
        ("zeta", true, false),
        ("alpha", true, false),
        ("off", false, false),
        ("team", true, true),
    ] {
        sqlx::query(
            "INSERT INTO oauth_apps (slug, provider, client_id, enabled, shared, description, allowed_scopes, created_by_id, created_at, updated_at) \
             VALUES ($1, 'google', 'cid', $2, $3, 'desc', '[\"s1\"]', $4, now(), now())")
            .bind(slug).bind(enabled).bind(shared).bind(admin).execute(&app.db.pool).await.unwrap();
    }
    let response = sandbox_call(
        &app,
        &token,
        Method::GET,
        "/api/v1/sandbox/oauth_apps",
        None,
    )
    .await;
    assert_eq!(response.status(), 200);
    let apps = response.json()["data"].as_array().unwrap().clone();
    let slugs: Vec<_> = apps.iter().map(|a| a["slug"].clone()).collect();
    assert_eq!(slugs, [json!("alpha"), json!("zeta")]);
    assert_eq!(
        apps[0]["start_url"],
        "https://console.example.test/oauth/alpha/start"
    );
    assert_eq!(apps[0]["allowed_scopes"], json!(["s1"]));
    assert!(apps[0]["id"].as_str().unwrap().starts_with("oap_"));
    assert!(!response.text().contains("client_secret"));
}
