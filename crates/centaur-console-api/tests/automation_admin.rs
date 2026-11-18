//! API clients, system settings, and the workflow dashboard (admin only).

mod automation_support;

use std::sync::atomic::Ordering;

use automation_support::*;
use axum::http::Method;
use centaur_console_api::{
    auth::Access,
    automation,
    testing::{TestApp, TestClient, TestResponse},
};
use serde_json::{Value, json};

const CLIENTS: &str = "/api/v1/console/api_clients";
const SETTINGS: &str = "/api/v1/console/system_settings";
const WORKFLOWS: &str = "/api/v1/console/workflows";

async fn app(env: &[(&str, &str)]) -> Option<(TestApp, std::sync::Arc<Api>, TestClient, i64)> {
    let (api, base) = stub_api().await;
    let mut vars = vec![("CENTAUR_CONSOLE_CENTAUR_API_URL", base.as_str())];
    vars.extend_from_slice(env);
    let app = TestApp::build_full(automation::routes(true), &vars, |_| {}, true).await?;
    let admin = app.create_user("admin@example.test", true, "active").await;
    let mut browser = app.client();
    browser.sign_in(admin.id);
    Some((app, api, browser, admin.id))
}

async fn send(
    app: &TestApp,
    browser: &mut TestClient,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> TestResponse {
    let mut request = app.request(method, path).csrf(browser);
    if let Some(body) = body {
        request = request.json(&body);
    }
    browser.send(request).await
}

// --- API clients ------------------------------------------------------------

#[tokio::test]
async fn api_client_routes_exist_only_while_the_feature_is_on() {
    let Some(off) = TestApp::new(automation::routes(false)).await else {
        return;
    };
    let admin = off.create_user("admin@example.test", true, "active").await;
    let mut browser = off.client();
    browser.sign_in(admin.id);
    let response = browser.send(off.get(CLIENTS)).await;
    assert_eq!(response.status(), 404);
    // The rest of the area is unaffected.
    assert_eq!(browser.send(off.get(SETTINGS)).await.status(), 200);
}

#[tokio::test]
async fn api_client_endpoints_are_admin_only() {
    let Some((app, ..)) = app(&[]).await else {
        return;
    };
    let body = json!({"data": {}});
    let id = centaur_console_db::oid("acl", 99);
    let link = centaur_console_db::oid("aclp", 99);
    for (method, path, body) in [
        (Method::GET, CLIENTS.to_owned(), None),
        (Method::POST, CLIENTS.to_owned(), Some(&body)),
        (Method::GET, format!("{CLIENTS}/{id}"), None),
        (Method::PATCH, format!("{CLIENTS}/{id}"), Some(&body)),
        (Method::PUT, format!("{CLIENTS}/{id}"), Some(&body)),
        (Method::DELETE, format!("{CLIENTS}/{id}"), None),
        (Method::POST, format!("{CLIENTS}/{id}/rotate_secret"), None),
        (Method::POST, format!("{CLIENTS}/{id}/disable"), None),
        (Method::POST, format!("{CLIENTS}/{id}/enable"), None),
        (
            Method::POST,
            format!("{CLIENTS}/{id}/principals"),
            Some(&body),
        ),
        (
            Method::DELETE,
            format!("{CLIENTS}/{id}/principals/{link}"),
            None,
        ),
        (
            Method::POST,
            format!("{CLIENTS}/{id}/principals/{link}/default"),
            None,
        ),
    ] {
        app.assert_console_access(Access::Admin, method, &path, body)
            .await;
    }
}

async fn create_client(app: &TestApp, browser: &mut TestClient, data: Value) -> TestResponse {
    send(
        app,
        browser,
        Method::POST,
        CLIENTS,
        Some(json!({"data": data})),
    )
    .await
}

async fn digest(app: &TestApp, id: &str) -> String {
    let id = centaur_console_db::decode_oid("acl", id).unwrap();
    sqlx::query_scalar("SELECT secret_digest FROM api_clients WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn an_api_client_is_created_with_a_secret_shown_once() {
    let Some((app, _, mut browser, admin)) = app(&[]).await else {
        return;
    };
    let principal = app.create_principal("workflow", "wf", admin).await;
    sqlx::query("UPDATE principals SET foreign_id = 'wf-1' WHERE id = $1")
        .bind(principal)
        .execute(&app.db.pool)
        .await
        .unwrap();

    let created = create_client(
        &app,
        &mut browser,
        json!({"name": " Ingress ", "scopes": ["sessions:read", "", "sessions:write"], "principal": "wf-1"}),
    )
    .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    assert_eq!(created.header("cache-control"), Some("no-store"));
    let client = created.json()["data"].clone();
    let secret = client["secret"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("acs_") && secret.len() > 20);
    assert_eq!(client["name"], "Ingress");
    assert_eq!(client["scopes"], json!(["sessions:read", "sessions:write"]));
    assert_eq!(client["disabled"], false);
    assert_eq!(client["principals_count"], 1);
    assert_eq!(client["principals"][0]["default"], true);
    assert_eq!(client["principals"][0]["principal"]["foreign_id"], "wf-1");
    assert_eq!(client["principals"][0]["principal"]["kind"], "workflow");
    let id = client["id"].as_str().unwrap().to_owned();
    // Only the digest is stored.
    assert_eq!(
        digest(&app, &id).await,
        centaur_console_auth::bearer::hash_token(&secret)
    );

    // Reads never carry the secret.
    for response in [
        send(
            &app,
            &mut browser,
            Method::GET,
            &format!("{CLIENTS}/{id}"),
            None,
        )
        .await,
        send(&app, &mut browser, Method::GET, CLIENTS, None).await,
    ] {
        assert_eq!(response.status(), 200);
        assert!(!response.text().contains(&secret));
    }
    let list = send(&app, &mut browser, Method::GET, CLIENTS, None).await;
    assert_eq!(list.json()["meta"]["total"], 1);

    let rotated = send(
        &app,
        &mut browser,
        Method::POST,
        &format!("{CLIENTS}/{id}/rotate_secret"),
        None,
    )
    .await;
    assert_eq!(rotated.status(), 200);
    assert_eq!(rotated.header("cache-control"), Some("no-store"));
    let new_secret = rotated.json()["data"]["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(new_secret, secret);
    assert_eq!(
        digest(&app, &id).await,
        centaur_console_auth::bearer::hash_token(&new_secret)
    );

    let deleted = send(
        &app,
        &mut browser,
        Method::DELETE,
        &format!("{CLIENTS}/{id}"),
        None,
    )
    .await;
    assert_eq!(deleted.status(), 204);
    let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_client_principals")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(links, 0);
    assert_eq!(
        send(
            &app,
            &mut browser,
            Method::GET,
            &format!("{CLIENTS}/{id}"),
            None
        )
        .await
        .status(),
        404
    );
}

#[tokio::test]
async fn invalid_api_clients_are_refused() {
    let Some((app, _, mut browser, _)) = app(&[]).await else {
        return;
    };
    let blank = create_client(&app, &mut browser, json!({"name": " ", "scopes": []})).await;
    assert_eq!(blank.status(), 422);
    let details = &blank.json()["error"]["details"];
    assert!(details["name"].is_array() && details["scopes"].is_array());
    let unsupported = create_client(
        &app,
        &mut browser,
        json!({"name": "x", "scopes": ["admin"]}),
    )
    .await;
    assert_eq!(
        unsupported.json()["error"]["details"]["scopes"][0],
        "contains unsupported values: admin"
    );
    let unknown = create_client(
        &app,
        &mut browser,
        json!({"name": "x", "scopes": ["sessions:read"], "principal": "nobody"}),
    )
    .await;
    assert_eq!(unknown.status(), 422);
    assert!(unknown.json()["error"]["details"]["base"].is_array());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_clients")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);

    let created = create_client(
        &app,
        &mut browser,
        json!({"name": "ok", "scopes": ["sessions:read"]}),
    )
    .await;
    let id = created.json()["data"]["id"].as_str().unwrap().to_owned();
    let bad_update = send(
        &app,
        &mut browser,
        Method::PATCH,
        &format!("{CLIENTS}/{id}"),
        Some(json!({"data": {"scopes": []}})),
    )
    .await;
    assert_eq!(bad_update.status(), 422);
    for path in ["nope", "acl_zzzzzzzz"] {
        assert_eq!(
            send(
                &app,
                &mut browser,
                Method::GET,
                &format!("{CLIENTS}/{path}"),
                None
            )
            .await
            .status(),
            404
        );
    }
}

#[tokio::test]
async fn disabling_enabling_and_editing_touch_only_their_columns() {
    let Some((app, _, mut browser, _)) = app(&[]).await else {
        return;
    };
    let created = create_client(
        &app,
        &mut browser,
        json!({"name": "ok", "scopes": ["sessions:read"]}),
    )
    .await;
    let id = created.json()["data"]["id"].as_str().unwrap().to_owned();
    let secret = created.json()["data"]["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("{CLIENTS}/{id}");

    let disabled = send(
        &app,
        &mut browser,
        Method::POST,
        &format!("{path}/disable"),
        None,
    )
    .await;
    assert_eq!(disabled.json()["data"]["disabled"], true);
    assert!(disabled.json()["data"]["disabled_at"].is_string());
    // Editing a disabled client keeps it disabled and keeps its secret.
    let edited = send(
        &app,
        &mut browser,
        Method::PATCH,
        &path,
        Some(json!({"data": {"name": "renamed", "scopes": ["sessions:write"]}})),
    )
    .await;
    assert_eq!(edited.json()["data"]["name"], "renamed");
    assert_eq!(edited.json()["data"]["scopes"], json!(["sessions:write"]));
    assert_eq!(edited.json()["data"]["disabled"], true);
    assert_eq!(
        digest(&app, &id).await,
        centaur_console_auth::bearer::hash_token(&secret)
    );
    let again = send(
        &app,
        &mut browser,
        Method::POST,
        &format!("{path}/disable"),
        None,
    )
    .await;
    assert_eq!(
        again.json()["data"]["disabled_at"],
        disabled.json()["data"]["disabled_at"]
    );
    let enabled = send(
        &app,
        &mut browser,
        Method::POST,
        &format!("{path}/enable"),
        None,
    )
    .await;
    assert_eq!(enabled.json()["data"]["disabled"], false);
}

/// A rename that was waiting for the client's row must not bring back a
/// secret or an enabled state that changed meanwhile.
#[tokio::test]
async fn an_edit_cannot_undo_a_concurrent_disable_or_rotation() {
    let Some((app, _, mut browser, admin)) = app(&[]).await else {
        return;
    };
    let created = create_client(
        &app,
        &mut browser,
        json!({"name": "ok", "scopes": ["sessions:read"]}),
    )
    .await;
    let id = created.json()["data"]["id"].as_str().unwrap().to_owned();
    let client_id = centaur_console_db::decode_oid("acl", &id).unwrap();

    let mut holder = app.db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM api_clients WHERE id = $1 FOR UPDATE")
        .bind(client_id)
        .execute(&mut *holder)
        .await
        .unwrap();
    let mut racer = app.client();
    racer.sign_in(admin);
    let request = app
        .request(Method::PATCH, &format!("{CLIENTS}/{id}"))
        .csrf(&racer)
        .json(&json!({"data": {"name": "renamed"}}));
    let rename = tokio::spawn(async move { racer.send(request).await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    sqlx::query(
        "UPDATE api_clients SET disabled_at = now(), secret_digest = 'rotated' WHERE id = $1",
    )
    .bind(client_id)
    .execute(&mut *holder)
    .await
    .unwrap();
    holder.commit().await.unwrap();
    assert_eq!(rename.await.unwrap().status(), 200);

    let (name, disabled, secret): (String, bool, String) = sqlx::query_as(
        "SELECT name, disabled_at IS NOT NULL, secret_digest FROM api_clients WHERE id = $1",
    )
    .bind(client_id)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(
        (name.as_str(), disabled, secret.as_str()),
        ("renamed", true, "rotated")
    );
}

/// Detach takes the client row lock like attach and make-default, so it cannot
/// delete a link while another request is deciding that client's default.
#[tokio::test]
async fn a_detach_waits_for_the_client_row_lock() {
    let Some((app, _, mut browser, admin)) = app(&[]).await else {
        return;
    };
    let first = app.create_principal("workflow", "first", admin).await;
    let second = app.create_principal("workflow", "second", admin).await;
    let created = create_client(
        &app,
        &mut browser,
        json!({"name": "ok", "scopes": ["sessions:read"]}),
    )
    .await;
    let id = created.json()["data"]["id"].as_str().unwrap().to_owned();
    let client_id = centaur_console_db::decode_oid("acl", &id).unwrap();
    let path = format!("{CLIENTS}/{id}/principals");
    let a = send(
        &app,
        &mut browser,
        Method::POST,
        &path,
        Some(json!({"data": {"principal": centaur_console_db::oid("prn", first)}})),
    )
    .await;
    assert_eq!(a.json()["data"]["default"], true);
    let b = send(
        &app,
        &mut browser,
        Method::POST,
        &path,
        Some(json!({"data": {"principal": centaur_console_db::oid("prn", second)}})),
    )
    .await;
    let a_id = a.json()["data"]["id"].as_str().unwrap().to_owned();
    assert_eq!(b.json()["data"]["default"], false);

    let mut holder = app.db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM api_clients WHERE id = $1 FOR UPDATE")
        .bind(client_id)
        .execute(&mut *holder)
        .await
        .unwrap();
    let mut racer = app.client();
    racer.sign_in(admin);
    let request = app
        .request(Method::DELETE, &format!("{path}/{a_id}"))
        .csrf(&racer);
    let mut detach = tokio::spawn(async move { racer.send(request).await });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), &mut detach)
            .await
            .is_err(),
        "the detach must wait for the client row lock"
    );
    holder.commit().await.unwrap();
    assert_eq!(detach.await.unwrap().status(), 204);
    let links: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM api_client_principals WHERE api_client_id = $1")
            .bind(client_id)
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(links, 1);
}

#[tokio::test]
async fn principals_attach_detach_and_default() {
    let Some((app, _, mut browser, admin)) = app(&[]).await else {
        return;
    };
    let first = app.create_principal("workflow", "first", admin).await;
    let second = app.create_principal("workflow", "second", admin).await;
    let created = create_client(
        &app,
        &mut browser,
        json!({"name": "ok", "scopes": ["sessions:read"]}),
    )
    .await;
    let id = created.json()["data"]["id"].as_str().unwrap().to_owned();
    let path = format!("{CLIENTS}/{id}/principals");

    let a = send(
        &app,
        &mut browser,
        Method::POST,
        &path,
        Some(json!({"data": {"principal": centaur_console_db::oid("prn", first)}})),
    )
    .await;
    assert_eq!(a.status(), 201, "{}", a.text());
    assert_eq!(a.json()["data"]["default"], true);
    let b = send(
        &app,
        &mut browser,
        Method::POST,
        &path,
        Some(json!({"data": {"principal": centaur_console_db::oid("prn", second)}})),
    )
    .await;
    assert_eq!(b.json()["data"]["default"], false);
    // Attaching again changes nothing.
    let again = send(
        &app,
        &mut browser,
        Method::POST,
        &path,
        Some(json!({"data": {"principal": centaur_console_db::oid("prn", first)}})),
    )
    .await;
    assert_eq!(again.json()["data"]["id"], a.json()["data"]["id"]);
    let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_client_principals")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(links, 2);

    let unknown = send(
        &app,
        &mut browser,
        Method::POST,
        &path,
        Some(json!({"data": {"principal": "prn_nope"}})),
    )
    .await;
    assert_eq!(unknown.status(), 422);

    let b_id = b.json()["data"]["id"].as_str().unwrap().to_owned();
    let promoted = send(
        &app,
        &mut browser,
        Method::POST,
        &format!("{path}/{b_id}/default"),
        None,
    )
    .await;
    assert_eq!(promoted.status(), 200);
    assert_eq!(promoted.json()["data"]["default"], true);
    let defaults: Vec<i64> =
        sqlx::query_scalar("SELECT principal_id FROM api_client_principals WHERE \"default\"")
            .fetch_all(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(defaults, [second]);

    let detached = send(
        &app,
        &mut browser,
        Method::DELETE,
        &format!("{path}/{b_id}"),
        None,
    )
    .await;
    assert_eq!(detached.status(), 204);
    assert_eq!(
        send(
            &app,
            &mut browser,
            Method::DELETE,
            &format!("{path}/{b_id}"),
            None
        )
        .await
        .status(),
        404
    );
    // Another client's link is not found through this one.
    let other = create_client(
        &app,
        &mut browser,
        json!({"name": "other", "scopes": ["sessions:read"]}),
    )
    .await;
    let other_id = other.json()["data"]["id"].as_str().unwrap().to_owned();
    let a_id = a.json()["data"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        send(
            &app,
            &mut browser,
            Method::DELETE,
            &format!("{CLIENTS}/{other_id}/principals/{a_id}"),
            None
        )
        .await
        .status(),
        404
    );
}

// --- system settings ----------------------------------------------------------

#[tokio::test]
async fn system_settings_are_admin_only() {
    let Some((app, ..)) = app(&[]).await else {
        return;
    };
    let body = json!({"data": {}});
    let teams = format!("{SETTINGS}/slack_external_teams");
    for (method, path, body) in [
        (Method::GET, SETTINGS.to_owned(), None),
        (Method::PATCH, SETTINGS.to_owned(), Some(&body)),
        (Method::PUT, SETTINGS.to_owned(), Some(&body)),
        (Method::GET, teams.clone(), None),
        (Method::PUT, teams, Some(&body)),
    ] {
        app.assert_console_access(Access::Admin, method, &path, body)
            .await;
    }
}

#[tokio::test]
async fn system_settings_are_read_and_updated() {
    let Some((app, _, mut browser, admin)) = app(&[]).await else {
        return;
    };
    let now = app.now().naive_utc();
    let role: i64 = sqlx::query_scalar(
        "INSERT INTO roles (name, foreign_id, created_by_id, created_at, updated_at) \
         VALUES ('Readers', 'readers', $1, $2, $2) RETURNING id",
    )
    .bind(admin)
    .bind(now)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    let role_oid = centaur_console_db::oid("role", role);

    let shown = send(&app, &mut browser, Method::GET, SETTINGS, None).await;
    assert_eq!(shown.status(), 200, "{}", shown.text());
    let data = &shown.json()["data"];
    assert_eq!(data["default_sandbox_repo_cache"], "all");
    assert_eq!(data["default_sandbox_observability_enabled"], true);
    assert_eq!(data["default_sandbox_sessions_read_enabled"], false);
    assert_eq!(data["default_role_ids"], json!([]));
    assert_eq!(data["roles"][0]["id"], role_oid);
    assert_eq!(data["roles"][0]["assign_by_default"], false);

    let updated = send(
        &app,
        &mut browser,
        Method::PATCH,
        SETTINGS,
        Some(json!({"data": {
            "default_sandbox_repo_cache": "public",
            "default_sandbox_sessions_read_enabled": true,
            "default_role_ids": [role_oid],
        }})),
    )
    .await;
    assert_eq!(updated.status(), 200, "{}", updated.text());
    let data = &updated.json()["data"];
    assert_eq!(data["default_sandbox_repo_cache"], "public");
    assert_eq!(data["default_sandbox_sessions_read_enabled"], true);
    assert_eq!(data["default_sandbox_observability_enabled"], true);
    assert_eq!(data["default_role_ids"], json!([role_oid]));

    // Absent fields stay; an empty list clears the default roles.
    let cleared = send(
        &app,
        &mut browser,
        Method::PUT,
        SETTINGS,
        Some(json!({"data": {"default_role_ids": []}})),
    )
    .await;
    assert_eq!(cleared.json()["data"]["default_role_ids"], json!([]));
    assert_eq!(
        cleared.json()["data"]["default_sandbox_repo_cache"],
        "public"
    );
}

#[tokio::test]
async fn invalid_system_settings_change_nothing() {
    let Some((app, _, mut browser, _)) = app(&[]).await else {
        return;
    };
    let invalid = send(
        &app,
        &mut browser,
        Method::PATCH,
        SETTINGS,
        Some(json!({"data": {"default_sandbox_repo_cache": "everything", "default_role_ids": ["role_nope"]}})),
    )
    .await;
    assert_eq!(invalid.status(), 422);
    let details = &invalid.json()["error"]["details"];
    assert!(details["default_sandbox_repo_cache"].is_array());
    assert_eq!(
        details["default_role_ids"][0],
        "contains unknown roles: role_nope"
    );
    let shown = send(&app, &mut browser, Method::GET, SETTINGS, None).await;
    assert_eq!(shown.json()["data"]["default_sandbox_repo_cache"], "all");
}

#[tokio::test]
async fn slack_connect_workspaces_live_in_api_rs() {
    let Some((app, api, mut browser, _)) = app(&[]).await else {
        return;
    };
    let path = format!("{SETTINGS}/slack_external_teams");
    api.teams.lock().unwrap().push("T0000000001".into());
    let listed = send(&app, &mut browser, Method::GET, &path, None).await;
    assert_eq!(
        listed.json()["data"]["slack_external_team_ids"],
        json!(["T0000000001"])
    );

    let replaced = send(
        &app,
        &mut browser,
        Method::PUT,
        &path,
        Some(json!({"data": {"slack_external_team_ids": "T0000000002, T0000000003\nT0000000002"}})),
    )
    .await;
    assert_eq!(replaced.status(), 200, "{}", replaced.text());
    let sent = api.calls_to("/external-team-ids");
    let put = sent.iter().find(|call| call.method == "PUT").unwrap();
    assert!(put.bearer);
    assert_eq!(
        put.body["slack_external_team_ids"],
        json!(["T0000000002", "T0000000003", "T0000000002"])
    );

    let rejected = send(
        &app,
        &mut browser,
        Method::PUT,
        &path,
        Some(json!({"data": {"slack_external_team_ids": ["bad"]}})),
    )
    .await;
    assert_eq!(rejected.status(), 422);
    assert_eq!(rejected.error_message().as_deref(), Some("invalid team id"));

    api.down.store(true, Ordering::SeqCst);
    assert_eq!(
        send(&app, &mut browser, Method::GET, &path, None)
            .await
            .status(),
        503
    );
    let down = send(
        &app,
        &mut browser,
        Method::PUT,
        &path,
        Some(json!({"data": {"slack_external_team_ids": []}})),
    )
    .await;
    assert_eq!(down.status(), 503);
}

// --- workflows ----------------------------------------------------------------

#[tokio::test]
async fn workflow_endpoints_are_admin_only() {
    let Some((app, ..)) = app(&[]).await else {
        return;
    };
    for (method, path) in [
        (Method::GET, WORKFLOWS.to_owned()),
        (Method::GET, format!("{WORKFLOWS}/daily")),
        (Method::GET, format!("{WORKFLOWS}/daily/runs")),
        (Method::POST, format!("{WORKFLOWS}/daily/runs")),
    ] {
        app.assert_console_access(Access::Admin, method, &path, None)
            .await;
    }
}

async fn workflow_view(app: &TestApp) {
    sqlx::query(
        "CREATE TABLE centaur_readonly_workflow_runs (
           queue_name text, run_id uuid, task_id uuid, task_name text, workflow_name text,
           harness_type text, state text, attempts int, max_attempts int,
           created_at timestamptz, first_started_at timestamptz, started_at timestamptz,
           completed_at timestamptz, failed_at timestamptz, available_at timestamptz,
           claimed boolean, cancelled_at timestamptz)",
    )
    .execute(&app.db.pool)
    .await
    .unwrap();
    for (workflow, queue, state, minutes_ago, failed) in [
        ("daily", "centaur_workflows", "completed", 50, false),
        ("daily", "centaur_workflows", "failed", 30, true),
        (
            "daily",
            "centaur_workflows_high_priority",
            "running",
            5,
            false,
        ),
        ("weekly", "centaur_workflows", "pending", 10, false),
    ] {
        sqlx::query(
            "INSERT INTO centaur_readonly_workflow_runs (queue_name, run_id, task_id, workflow_name, \
             harness_type, state, attempts, max_attempts, created_at, started_at, completed_at, failed_at) \
             VALUES ($1, gen_random_uuid(), gen_random_uuid(), $2, 'claudecode', $3, 1, 3, \
                     now() - make_interval(mins => $4), now() - make_interval(mins => $4), \
                     CASE WHEN $3 = 'completed' THEN now() - make_interval(mins => $4 - 1) END, \
                     CASE WHEN $5 THEN now() - make_interval(mins => $4 - 1) END)",
        )
        .bind(queue)
        .bind(workflow)
        .bind(state)
        .bind(minutes_ago)
        .bind(failed)
        .execute(&app.db.pool)
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn the_workflow_dashboard_reads_the_run_view() {
    let Some((app, api, mut browser, _)) = app(&[]).await else {
        return;
    };
    *api.schedules.lock().unwrap() = json!([{
        "workflow_name": "daily",
        "kind": {"type": "cron", "cron": "0 9 * * *"},
        "timezone": "UTC",
        "source_path": "workflows/daily.py",
        "input": {"channel": "C1"},
    }]);

    // Without the view the dashboard is unavailable.
    let unavailable = send(&app, &mut browser, Method::GET, WORKFLOWS, None).await;
    assert_eq!(unavailable.status(), 503);
    assert_eq!(
        unavailable.error_reason().as_deref(),
        Some("workflow_database_unavailable")
    );
    workflow_view(&app).await;

    let index = send(&app, &mut browser, Method::GET, WORKFLOWS, None).await;
    assert_eq!(index.status(), 200, "{}", index.text());
    assert_eq!(index.header("cache-control"), Some("no-store"));
    let rows = index.json()["data"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 2);
    // Newest activity first.
    assert_eq!(rows[0]["workflow_key"], "daily");
    assert_eq!(rows[0]["status"], "running");
    assert_eq!(rows[0]["queues"].as_array().unwrap().len(), 2);
    assert_eq!(rows[1]["workflow_key"], "weekly");
    assert_eq!(index.json()["meta"]["total"], 2);

    let detail = send(
        &app,
        &mut browser,
        Method::GET,
        &format!("{WORKFLOWS}/daily"),
        None,
    )
    .await;
    assert_eq!(detail.status(), 200, "{}", detail.text());
    let data = &detail.json()["data"];
    assert_eq!(data["engine_label"], "Claude Code");
    assert_eq!(data["run_count"], 3);
    assert_eq!(
        data["status_counts"][0],
        json!({"status": "running", "count": 1})
    );
    assert_eq!(data["schedule"]["summary"], "cron 0 9 * * * · UTC");
    assert_eq!(
        data["schedule"]["source_url"],
        "https://github.com/paradigmxyz/centaur/blob/main/workflows/daily.py"
    );
    assert_eq!(data["debug"][0]["label"], "Input");
    assert!(data.get("duration_label").is_none());
    let unknown = send(
        &app,
        &mut browser,
        Method::GET,
        &format!("{WORKFLOWS}/nope"),
        None,
    )
    .await;
    assert_eq!(unknown.status(), 404);

    let runs = send(
        &app,
        &mut browser,
        Method::GET,
        &format!("{WORKFLOWS}/daily/runs?status=failed"),
        None,
    )
    .await;
    assert_eq!(runs.json()["data"].as_array().unwrap().len(), 1);
    assert_eq!(runs.json()["data"][0]["status"], "failed");
    let queue = send(
        &app,
        &mut browser,
        Method::GET,
        &format!("{WORKFLOWS}/daily/runs?queue=centaur_workflows_high_priority&limit=1"),
        None,
    )
    .await;
    assert_eq!(queue.json()["meta"]["total"], 1);
    assert_eq!(queue.json()["data"][0]["queue_label"], "high priority");

    // Force start: scheduled workflows start with their schedule's input.
    let started = send(
        &app,
        &mut browser,
        Method::POST,
        &format!("{WORKFLOWS}/daily/runs"),
        None,
    )
    .await;
    assert_eq!(started.status(), 201);
    assert_eq!(
        started.json()["data"],
        json!({"run_id": "run-1", "created": true})
    );
    let start = api
        .calls_to("/api/workflows/runs")
        .into_iter()
        .find(|call| call.method == "POST")
        .unwrap();
    assert_eq!(
        start.body,
        json!({"workflow_name": "daily", "input": {"channel": "C1"}})
    );
    let existing = send(
        &app,
        &mut browser,
        Method::POST,
        &format!("{WORKFLOWS}/existing/runs"),
        None,
    )
    .await;
    assert_eq!(existing.status(), 200);
    assert_eq!(existing.json()["data"]["created"], false);

    api.down.store(true, Ordering::SeqCst);
    let failed = send(
        &app,
        &mut browser,
        Method::POST,
        &format!("{WORKFLOWS}/daily/runs"),
        None,
    )
    .await;
    assert_eq!(failed.status(), 502);
    assert_eq!(failed.error_reason().as_deref(), Some("workflow_api_error"));
}
