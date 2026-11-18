//! Scheduled tasks: the console, bearer, and sandbox endpoints.

mod automation_support;

use std::sync::atomic::Ordering;

use automation_support::*;
use axum::http::Method;
use centaur_console_api::{
    auth::Access,
    automation,
    testing::{TestApp, TestClient},
};
use serde_json::{Value, json};

const BASE: &str = "/api/v1/console/scheduled_tasks";

async fn app() -> Option<(TestApp, std::sync::Arc<Api>)> {
    let (api, base) = stub_api().await;
    let app = TestApp::with_env(
        automation::routes(false),
        &[("CENTAUR_CONSOLE_CENTAUR_API_URL", &base)],
    )
    .await?;
    Some((app, api))
}

async fn member(app: &TestApp, email: &str) -> (i64, TestClient) {
    let user = app.create_user(email, false, "active").await;
    slack_identity(app, user.id, SLACK_USER, TEAM).await;
    let mut browser = app.client();
    browser.sign_in(user.id);
    (user.id, browser)
}

async fn call(
    app: &TestApp,
    browser: &mut TestClient,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> centaur_console_api::testing::TestResponse {
    let mut request = app.request(method, path).csrf(browser);
    if let Some(body) = body {
        request = request.json(&body);
    }
    browser.send(request).await
}

fn form(name: &str) -> Value {
    json!({"data": {
        "name": name,
        "prompt": "Summarize open incidents.",
        "delivery_mode": "channel",
        "delivery_channel": CHANNEL,
        "schedule_preset": "daily",
    }})
}

#[tokio::test]
async fn console_endpoints_are_for_active_users_only() {
    let Some((app, _)) = app().await else { return };
    let missing = task_oid(999_999);
    let body = json!({"data": {}});
    for (method, path, body) in [
        (Method::GET, BASE.to_owned(), None),
        (Method::POST, BASE.to_owned(), Some(&body)),
        (Method::GET, format!("{BASE}/new"), None),
        (Method::GET, format!("{BASE}/slack_channel_options"), None),
        (Method::GET, format!("{BASE}/{missing}/edit"), None),
        (Method::PATCH, format!("{BASE}/{missing}"), Some(&body)),
        (Method::PUT, format!("{BASE}/{missing}"), Some(&body)),
        (Method::DELETE, format!("{BASE}/{missing}"), None),
        (Method::POST, format!("{BASE}/{missing}/run"), None),
    ] {
        app.assert_console_access(Access::Active, method, &path, body)
            .await;
    }
}

#[tokio::test]
async fn a_user_creates_edits_and_deletes_their_task() {
    let Some((app, api)) = app().await else {
        return;
    };
    let (user_id, mut browser) = member(&app, "ada@example.test").await;
    bot_channel(&app, CHANNEL, TEAM, "incidents", false, &[]).await;

    let created = call(
        &app,
        &mut browser,
        Method::POST,
        BASE,
        Some(form("Incidents")),
    )
    .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    assert_eq!(created.header("cache-control"), Some("no-store"));
    let task = created.json()["data"].clone();
    let id = task["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("tsk_"));
    assert_eq!(task["author_id"], centaur_console_db::oid("usr", user_id));
    assert_eq!(task["name"], "Incidents");
    assert_eq!(task["delivery_channel"], CHANNEL);
    assert_eq!(task["delivery_mode"], "channel");
    assert_eq!(task["delivery_name"], "#incidents");
    assert_eq!(task["schedule_preset"], "daily");
    assert_eq!(task["cron_expression"], "0 9 * * *");
    assert_eq!(task["timezone"], "America/Los_Angeles");
    assert_eq!(task["enabled"], true);
    assert!(task["next_run_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(task["last_run_at"], Value::Null);
    // api-rs was told, with the console service token.
    let hints = api.calls_to("/reconcile");
    assert_eq!(hints.len(), 1);
    assert!(hints[0].bearer && hints[0].path.contains(&id));

    let list = call(&app, &mut browser, Method::GET, BASE, None).await;
    assert_eq!(list.json()["data"].as_array().unwrap().len(), 1);

    let edit = call(
        &app,
        &mut browser,
        Method::GET,
        &format!("{BASE}/{id}/edit"),
        None,
    )
    .await;
    assert_eq!(edit.json()["data"]["id"], id);

    // A rename only changes the name: the schedule is untouched.
    let before = task["next_run_at"].clone();
    let renamed = call(
        &app,
        &mut browser,
        Method::PATCH,
        &format!("{BASE}/{id}"),
        Some(json!({"data": {"name": "Renamed"}})),
    )
    .await;
    assert_eq!(renamed.status(), 200, "{}", renamed.text());
    assert_eq!(renamed.json()["data"]["name"], "Renamed");
    assert_eq!(renamed.json()["data"]["next_run_at"], before);

    // Switching to manual-only clears the schedule; disabling and enabling
    // recompute it.
    let manual = call(
        &app,
        &mut browser,
        Method::PUT,
        &format!("{BASE}/{id}"),
        Some(json!({"data": {"schedule_preset": "manual"}})),
    )
    .await;
    assert_eq!(manual.json()["data"]["schedule_preset"], "manual");
    assert_eq!(manual.json()["data"]["next_run_at"], Value::Null);
    let weekdays = call(
        &app,
        &mut browser,
        Method::PATCH,
        &format!("{BASE}/{id}"),
        Some(json!({"data": {"schedule_preset": "weekdays"}})),
    )
    .await;
    assert!(weekdays.json()["data"]["next_run_at"].is_string());
    let disabled = call(
        &app,
        &mut browser,
        Method::PATCH,
        &format!("{BASE}/{id}"),
        Some(json!({"data": {"enabled": false}})),
    )
    .await;
    assert_eq!(disabled.json()["data"]["next_run_at"], Value::Null);
    let enabled = call(
        &app,
        &mut browser,
        Method::PATCH,
        &format!("{BASE}/{id}"),
        Some(json!({"data": {"enabled": true}})),
    )
    .await;
    assert!(enabled.json()["data"]["next_run_at"].is_string());

    let deleted = call(
        &app,
        &mut browser,
        Method::DELETE,
        &format!("{BASE}/{id}"),
        None,
    )
    .await;
    assert_eq!(deleted.status(), 204);
    let gone: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_tasks")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(gone, 0);
    assert_eq!(api.calls_to("/reconcile").len(), 7);
}

#[tokio::test]
async fn dm_delivery_uses_the_users_slack_identity() {
    let Some((app, _)) = app().await else { return };
    let (_, mut browser) = member(&app, "ada@example.test").await;
    let created = call(
        &app,
        &mut browser,
        Method::POST,
        BASE,
        Some(json!({"data": {
            "name": "Digest", "prompt": "Digest.", "delivery_mode": "dm",
            "schedule_preset": "custom", "custom_days": ["1", "3"], "custom_time": "07:30",
        }})),
    )
    .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    let task = &created.json()["data"];
    assert_eq!(task["delivery_channel"], SLACK_USER);
    assert_eq!(task["delivery_mode"], "dm");
    assert_eq!(task["delivery_name"], "Direct message to you");
    assert_eq!(task["direct_message_user_id"], SLACK_USER);
    assert_eq!(task["cron_expression"], "30 7 * * 1,3");
    assert_eq!(task["schedule_preset"], "custom");
    assert_eq!(task["custom_days"], json!(["1", "3"]));
    assert_eq!(task["custom_time"], "07:30");
}

#[tokio::test]
async fn invalid_tasks_are_refused_with_field_errors() {
    let Some((app, api)) = app().await else {
        return;
    };
    let (user_id, mut browser) = member(&app, "ada@example.test").await;
    bot_channel(&app, CHANNEL, TEAM, "incidents", false, &[]).await;
    bot_channel(&app, "C0000000099", TEAM, "secret", true, &["UBOT00000"]).await;

    let blank = call(
        &app,
        &mut browser,
        Method::POST,
        BASE,
        Some(
            json!({"data": {"name": " ", "prompt": "", "delivery_mode": "channel",
                              "delivery_channel": "nope", "schedule_preset": "cron",
                              "cron_expression": "not a cron"}}),
        ),
    )
    .await;
    assert_eq!(blank.status(), 422);
    let details = &blank.json()["error"]["details"];
    for field in ["name", "prompt", "delivery_channel", "cron_expression"] {
        assert!(details[field].is_array(), "{field}: {details}");
    }

    // A private channel the user is not in is not available to them.
    let private = call(
        &app,
        &mut browser,
        Method::POST,
        BASE,
        Some(
            json!({"data": {"name": "N", "prompt": "P", "delivery_mode": "channel",
                              "delivery_channel": "C0000000099"}}),
        ),
    )
    .await;
    assert_eq!(private.status(), 422);
    assert_eq!(
        private.json()["error"]["details"]["delivery_channel"][0],
        "is not available to the author"
    );

    let first = call(&app, &mut browser, Method::POST, BASE, Some(form("Same"))).await;
    assert_eq!(first.status(), 201);
    let duplicate = call(&app, &mut browser, Method::POST, BASE, Some(form("Same"))).await;
    assert_eq!(duplicate.status(), 422);
    assert_eq!(
        duplicate.json()["error"]["details"]["name"][0],
        "has already been taken"
    );
    // Nothing was written or announced for the refused requests.
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_tasks WHERE author_id = $1")
        .bind(user_id)
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    assert_eq!(api.calls_to("/reconcile").len(), 1);

    let no_data = call(&app, &mut browser, Method::POST, BASE, Some(json!({}))).await;
    assert_eq!(no_data.status(), 400);
}

#[tokio::test]
async fn tasks_belong_to_their_author() {
    let Some((app, _)) = app().await else { return };
    let (_, mut ada) = member(&app, "ada@example.test").await;
    bot_channel(&app, CHANNEL, TEAM, "incidents", false, &[]).await;
    let created = call(&app, &mut ada, Method::POST, BASE, Some(form("Ada's"))).await;
    let id = created.json()["data"]["id"].as_str().unwrap().to_owned();

    let other = app.create_user("grace@example.test", true, "active").await;
    let mut grace = app.client();
    grace.sign_in(other.id);
    let list = call(&app, &mut grace, Method::GET, BASE, None).await;
    assert_eq!(list.json()["data"], json!([]));
    for (method, path) in [
        (Method::GET, format!("{BASE}/{id}/edit")),
        (Method::PATCH, format!("{BASE}/{id}")),
        (Method::DELETE, format!("{BASE}/{id}")),
        (Method::POST, format!("{BASE}/{id}/run")),
    ] {
        let response = call(&app, &mut grace, method, &path, Some(json!({"data": {}}))).await;
        assert_eq!(response.status(), 404, "{path}");
    }
    let malformed = call(
        &app,
        &mut ada,
        Method::GET,
        &format!("{BASE}/nope/edit"),
        None,
    )
    .await;
    assert_eq!(malformed.status(), 404);
    let still: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_tasks")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(still, 1);
}

/// Edits write only the columns they change, under the row lock: a claim or
/// result that committed first is never undone.
#[tokio::test]
async fn an_edit_never_overwrites_what_the_scheduler_committed() {
    let Some((app, _)) = app().await else { return };
    let (user_id, _browser) = member(&app, "ada@example.test").await;
    bot_channel(&app, CHANNEL, TEAM, "incidents", false, &[]).await;
    let id = insert_task(&app, user_id, "Hourly", Some("0 * * * *")).await;
    let oid = task_oid(id);
    let (old_next,): (chrono::NaiveDateTime,) =
        sqlx::query_as("SELECT next_run_at FROM scheduled_tasks WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db.pool)
            .await
            .unwrap();

    // The scheduler claims while the rename is waiting for the row.
    let mut holder = app.db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM scheduled_tasks WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut *holder)
        .await
        .unwrap();
    let path = format!("{BASE}/{oid}");
    let mut racer = app.client();
    racer.sign_in(user_id);
    let request = app
        .request(Method::PATCH, &path)
        .csrf(&racer)
        .json(&json!({"data": {"name": "Renamed"}}));
    let rename = tokio::spawn(async move { racer.send(request).await });
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let claimed_next = old_next + chrono::Duration::hours(1);
    sqlx::query(
        "UPDATE scheduled_tasks SET next_run_at = $2, last_enqueued_at = $3, last_run_id = 'run-9', \
         last_run_at = $3, last_error = 'boom' WHERE id = $1",
    )
    .bind(id)
    .bind(claimed_next)
    .bind(old_next)
    .execute(&mut *holder)
    .await
    .unwrap();
    holder.commit().await.unwrap();
    let response = rename.await.unwrap();
    assert_eq!(response.status(), 200, "{}", response.text());

    let (name, next, enqueued, run_id, error): (
        String,
        Option<chrono::NaiveDateTime>,
        Option<chrono::NaiveDateTime>,
        Option<String>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT name, next_run_at, last_enqueued_at, last_run_id, last_error \
         FROM scheduled_tasks WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(name, "Renamed");
    assert_eq!(next, Some(claimed_next));
    assert_eq!(enqueued, Some(old_next));
    assert_eq!(run_id.as_deref(), Some("run-9"));
    assert_eq!(error.as_deref(), Some("boom"));
}

#[tokio::test]
async fn run_now_asks_api_rs_and_reports_what_happened() {
    let Some((app, api)) = app().await else {
        return;
    };
    let (user_id, mut browser) = member(&app, "ada@example.test").await;
    bot_channel(&app, CHANNEL, TEAM, "incidents", false, &[]).await;
    let id = insert_task(&app, user_id, "Hourly", Some("0 * * * *")).await;
    let path = format!("{BASE}/{}/run", task_oid(id));

    let queued = call(&app, &mut browser, Method::POST, &path, None).await;
    assert_eq!(queued.status(), 202, "{}", queued.text());
    assert_eq!(queued.json()["data"]["queued"], true);
    assert_eq!(api.calls_to("/run").len(), 1);

    api.run_status.store(500, Ordering::SeqCst);
    let failed = call(&app, &mut browser, Method::POST, &path, None).await;
    assert_eq!(failed.status(), 503);

    sqlx::query("UPDATE scheduled_tasks SET enabled = FALSE WHERE id = $1")
        .bind(id)
        .execute(&app.db.pool)
        .await
        .unwrap();
    let disabled = call(&app, &mut browser, Method::POST, &path, None).await;
    assert_eq!(disabled.status(), 422);
    assert_eq!(disabled.error_reason().as_deref(), Some("task_disabled"));
    assert_eq!(api.calls_to("/run").len(), 2);
}

#[tokio::test]
async fn channel_options_follow_the_delivery_policy() {
    let Some((app, _)) = app().await else { return };
    let (user_id, mut browser) = member(&app, "ada@example.test").await;
    bot_channel(&app, "C0000000001", TEAM, "general", false, &[]).await;
    bot_channel(
        &app,
        "C0000000002",
        TEAM,
        "ours",
        true,
        &["UBOT00000", SLACK_USER],
    )
    .await;
    bot_channel(
        &app,
        "C0000000003",
        TEAM,
        "theirs",
        true,
        &["UBOT00000", "U0000000009"],
    )
    .await;
    bot_channel(&app, "C0000000004", "T9999999999", "elsewhere", false, &[]).await;
    let id = insert_task(&app, user_id, "Mine", None).await;

    let options = call(
        &app,
        &mut browser,
        Method::GET,
        &format!("{BASE}/slack_channel_options"),
        None,
    )
    .await;
    assert_eq!(options.status(), 200);
    let names: Vec<String> = options.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|option| option["label"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, ["#general", "#ours"]);
    assert_eq!(options.json()["data"][1]["private"], true);
    assert_eq!(
        options.json()["data"][1]["description"],
        "C0000000002 · Private"
    );
    assert_eq!(
        options.json()["meta"]["catalog_error"],
        "SLACK_BOT_TOKEN is not configured."
    );

    let filtered = call(
        &app,
        &mut browser,
        Method::GET,
        &format!(
            "{BASE}/slack_channel_options?q=our&task_id={}",
            task_oid(id)
        ),
        None,
    )
    .await;
    assert_eq!(filtered.json()["data"].as_array().unwrap().len(), 1);
    let foreign = call(
        &app,
        &mut browser,
        Method::GET,
        &format!(
            "{BASE}/slack_channel_options?task_id={}",
            task_oid(id + 1000)
        ),
        None,
    )
    .await;
    assert_eq!(foreign.status(), 404);
}

#[tokio::test]
async fn a_new_task_form_is_blank_with_a_daily_schedule() {
    let Some((app, _)) = app().await else { return };
    let (_, mut browser) = member(&app, "ada@example.test").await;
    let blank = call(
        &app,
        &mut browser,
        Method::GET,
        &format!("{BASE}/new"),
        None,
    )
    .await;
    let task = &blank.json()["data"];
    assert_eq!(task["id"], Value::Null);
    assert_eq!(task["schedule_preset"], "daily");
    assert_eq!(task["delivery_mode"], "dm");
    assert_eq!(task["enabled"], true);
}

// --- bearer -----------------------------------------------------------------

#[tokio::test]
async fn the_bearer_lookup_reports_execution_state() {
    let Some((app, _)) = app().await else { return };
    let admin = app.create_user("admin@example.test", true, "active").await;
    let id = insert_task(&app, admin.id, "Mine", None).await;
    let path = format!("/api/v1/scheduled_tasks/{}", task_oid(id));
    app.assert_bearer_access(Method::GET, &path, None).await;

    let key = app.create_api_key(admin.id).await;
    let found = app.get(&path).bearer(&key).send().await;
    assert_eq!(found.status(), 200);
    assert_eq!(
        found.json()["data"],
        json!({"id": task_oid(id), "enabled": true, "delivery_channel": CHANNEL})
    );
    let missing = app
        .get(&format!("/api/v1/scheduled_tasks/{}", task_oid(id + 1)))
        .bearer(&key)
        .send()
        .await;
    assert_eq!(missing.status(), 404);
}

// --- sandbox ----------------------------------------------------------------

struct Sandbox {
    token: String,
    user_id: i64,
}

async fn console_user_sandbox(app: &TestApp, email: &str, slack_user: &str) -> Sandbox {
    let user = app.create_user(email, false, "active").await;
    slack_identity(app, user.id, slack_user, TEAM).await;
    let principal = app.create_principal("console_user", email, user.id).await;
    sqlx::query("UPDATE principals SET console_user_id = $2 WHERE id = $1")
        .bind(principal)
        .bind(user.id)
        .execute(&app.db.pool)
        .await
        .unwrap();
    let proxy = app
        .create_proxy(&format!("sbx-{principal}"), principal)
        .await;
    Sandbox {
        token: app.sandbox_token(proxy, principal).await,
        user_id: user.id,
    }
}

const SANDBOX: &str = "/api/v1/sandbox/scheduled_tasks";

#[tokio::test]
async fn sandbox_routes_need_an_entitlement_token() {
    let Some((app, _)) = app().await else { return };
    let anonymous = app.get(SANDBOX).send().await;
    assert_eq!(anonymous.status(), 401);
    let forged = app.get(SANDBOX).bearer("not-a-jwt").send().await;
    assert_eq!(forged.status(), 401);
    let admin = app.create_user("admin@example.test", true, "active").await;
    let key = app.create_api_key(admin.id).await;
    assert_eq!(app.get(SANDBOX).bearer(&key).send().await.status(), 401);
    let mut browser = app.client();
    browser.sign_in(admin.id);
    assert_eq!(browser.send(app.get(SANDBOX)).await.status(), 401);
}

#[tokio::test]
async fn a_sandbox_manages_its_console_users_tasks() {
    let Some((app, api)) = app().await else {
        return;
    };
    let sandbox = console_user_sandbox(&app, "ada@example.test", SLACK_USER).await;
    bot_channel(&app, CHANNEL, TEAM, "incidents", false, &[]).await;

    // Omitting the delivery channel means the user's Slack DM; a null cron is
    // an ad hoc task.
    let created = app
        .post(SANDBOX)
        .bearer(&sandbox.token)
        .json(&json!({"data": {"name": "Adhoc", "prompt": "Do it.", "cron_expression": null, "enabled": true}}))
        .send()
        .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    assert_eq!(created.header("cache-control"), Some("no-store"));
    let task = created.json()["data"].clone();
    assert_eq!(task["delivery_channel"], SLACK_USER);
    assert_eq!(task["cron_expression"], Value::Null);
    assert_eq!(task["schedule_label"], "Manual only");
    assert_eq!(task["next_run_at"], Value::Null);
    let id = task["id"].as_str().unwrap().to_owned();
    assert_eq!(api.calls_to("/reconcile").len(), 1);

    let scheduled = app
        .patch(&format!("{SANDBOX}/{id}"))
        .bearer(&sandbox.token)
        .json(&json!({"data": {"cron_expression": "0 9 * * 1-5", "delivery_channel": CHANNEL}}))
        .send()
        .await;
    assert_eq!(scheduled.status(), 200, "{}", scheduled.text());
    let task = &scheduled.json()["data"];
    assert_eq!(task["schedule_label"], "Weekdays at 9:00 PT");
    assert_eq!(task["delivery_channel"], CHANNEL);
    assert!(task["next_run_at"].is_string());

    let back_to_dm = app
        .patch(&format!("{SANDBOX}/{id}"))
        .bearer(&sandbox.token)
        .json(&json!({"data": {"delivery_channel": "dm"}}))
        .send()
        .await;
    assert_eq!(back_to_dm.json()["data"]["delivery_channel"], SLACK_USER);

    let listed = app.get(SANDBOX).bearer(&sandbox.token).send().await;
    assert_eq!(listed.json()["data"].as_array().unwrap().len(), 1);
    let shown = app
        .get(&format!("{SANDBOX}/{id}"))
        .bearer(&sandbox.token)
        .send()
        .await;
    assert_eq!(shown.json()["data"]["name"], "Adhoc");

    let run = app
        .post(&format!("{SANDBOX}/{id}/run"))
        .bearer(&sandbox.token)
        .send()
        .await;
    assert_eq!(run.status(), 202);
    assert_eq!(run.json()["data"]["queued"], true);
    sqlx::query("UPDATE scheduled_tasks SET enabled = FALSE")
        .execute(&app.db.pool)
        .await
        .unwrap();
    let disabled = app
        .post(&format!("{SANDBOX}/{id}/run"))
        .bearer(&sandbox.token)
        .send()
        .await;
    assert_eq!(disabled.status(), 422);

    // Another user's sandbox cannot see it.
    let other = console_user_sandbox(&app, "grace@example.test", "U0000000002").await;
    assert_eq!(
        app.get(&format!("{SANDBOX}/{id}"))
            .bearer(&other.token)
            .send()
            .await
            .status(),
        404
    );
    assert_eq!(
        app.delete(&format!("{SANDBOX}/{id}"))
            .bearer(&other.token)
            .send()
            .await
            .status(),
        404
    );
    assert_eq!(
        app.get(SANDBOX).bearer(&other.token).send().await.json()["data"],
        json!([])
    );

    let deleted = app
        .delete(&format!("{SANDBOX}/{id}"))
        .bearer(&sandbox.token)
        .send()
        .await;
    assert_eq!(deleted.status(), 204);
    let _ = sandbox.user_id;
}

#[tokio::test]
async fn a_sandbox_without_an_active_console_user_is_refused() {
    let Some((app, _)) = app().await else { return };
    let admin = app.create_user("admin@example.test", true, "active").await;
    let principal = app.create_principal("workflow", "wf", admin.id).await;
    let proxy = app.create_proxy("sbx-wf", principal).await;
    let token = app.sandbox_token(proxy, principal).await;
    let refused = app.get(SANDBOX).bearer(&token).send().await;
    assert_eq!(refused.status(), 403);
    let linked = console_user_sandbox(&app, "ada@example.test", SLACK_USER).await;
    sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
        .bind(linked.user_id)
        .execute(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(
        app.get(SANDBOX).bearer(&linked.token).send().await.status(),
        403
    );
}

#[tokio::test]
async fn channel_tasks_need_a_slack_requester_and_always_deliver_to_the_channel() {
    let Some((app, _)) = app().await else { return };
    let admin = app.create_user("admin@example.test", true, "active").await;
    bot_channel(&app, CHANNEL, TEAM, "incidents", false, &[]).await;
    let channel = app
        .create_principal("slack_channel", "#incidents", admin.id)
        .await;
    sqlx::query("UPDATE principals SET slack_channel_id = $2, slack_team_id = $3 WHERE id = $1")
        .bind(channel)
        .bind(CHANNEL)
        .bind(TEAM)
        .execute(&app.db.pool)
        .await
        .unwrap();
    let proxy = app.create_proxy("sbx-channel", channel).await;
    let token = app.sandbox_token(proxy, channel).await;

    // No Slack user behind the request.
    let refused = app.get(SANDBOX).bearer(&token).send().await;
    assert_eq!(refused.status(), 403);
    assert_eq!(
        refused.error_message().as_deref(),
        Some("channel tasks require a Slack user request")
    );

    let requester = app.create_principal("slack_dm", "dm", admin.id).await;
    sqlx::query("UPDATE principals SET slack_user_id = $2, slack_team_id = $3 WHERE id = $1")
        .bind(requester)
        .bind(SLACK_USER)
        .bind(TEAM)
        .execute(&app.db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE proxies SET requester_principal_id = $2 WHERE id = $1")
        .bind(proxy)
        .bind(requester)
        .execute(&app.db.pool)
        .await
        .unwrap();

    let elsewhere = app
        .post(SANDBOX)
        .bearer(&token)
        .json(&json!({"data": {"name": "N", "prompt": "P", "delivery_channel": "C0000000042"}}))
        .send()
        .await;
    assert_eq!(elsewhere.status(), 422);
    assert_eq!(
        elsewhere.error_message().as_deref(),
        Some("tasks created in a Slack channel thread always deliver to that channel")
    );

    let created = app
        .post(SANDBOX)
        .bearer(&token)
        .json(&json!({"data": {"name": "Channel task", "prompt": "P", "cron_expression": "0 9 * * *"}}))
        .send()
        .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    assert_eq!(created.json()["data"]["delivery_channel"], CHANNEL);
    let row: (Option<i64>, Option<i64>) =
        sqlx::query_as("SELECT author_id, principal_id FROM scheduled_tasks")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(row, (None, Some(channel)));
}
