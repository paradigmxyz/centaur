//! The runner's claim and result endpoints (`/internal/v1/scheduled_tasks`).

mod automation_support;

use automation_support::*;
use centaur_console_api::{
    automation,
    testing::{TestApp, TestResponse},
};
use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde_json::{Value, json};

async fn app() -> Option<TestApp> {
    TestApp::new(automation::routes(false)).await
}

fn path(id: i64, action: &str) -> String {
    format!("/internal/v1/scheduled_tasks/{}/{action}", task_oid(id))
}

async fn post(app: &TestApp, path: &str, body: &Value) -> TestResponse {
    app.post(path)
        .bearer(&app.internal_token())
        .json(body)
        .send()
        .await
}

fn wire(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// An hourly UTC task of `author` due at `due`.
async fn due_task(app: &TestApp, author: i64, due: DateTime<Utc>) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO scheduled_tasks (author_id, name, prompt, delivery_channel, cron_expression, \
         timezone, next_run_at, created_at, updated_at) \
         VALUES ($1, 'Hourly', 'Summarize.', $2, '0 * * * *', 'Etc/UTC', $3, $4, $4) RETURNING id",
    )
    .bind(author)
    .bind(CHANNEL)
    .bind(due.naive_utc())
    .bind(app.now().naive_utc())
    .fetch_one(&app.db.pool)
    .await
    .unwrap()
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
    next_run_at: Option<NaiveDateTime>,
    last_enqueued_at: Option<NaiveDateTime>,
    last_run_id: Option<String>,
    last_run_at: Option<NaiveDateTime>,
    last_error: Option<String>,
    name: String,
}

async fn row(app: &TestApp, id: i64) -> Row {
    sqlx::query_as(
        "SELECT next_run_at, last_enqueued_at, last_run_id, last_run_at, last_error, name \
         FROM scheduled_tasks WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&app.db.pool)
    .await
    .unwrap()
}

async fn setup(app: &TestApp) -> (i64, DateTime<Utc>) {
    let author = app.create_user("ada@example.test", false, "active").await;
    slack_identity(app, author.id, SLACK_USER, TEAM).await;
    bot_channel(app, CHANNEL, TEAM, "incidents", false, &[]).await;
    let due = app.now() - Duration::minutes(5);
    (due_task(app, author.id, due).await, due)
}

#[tokio::test]
async fn only_api_rs_may_call_the_runner_endpoints() {
    let Some(app) = app().await else {
        return;
    };
    let (id, due) = setup(&app).await;
    let body = json!({"scheduled_for": wire(due)});
    for action in ["claim", "result"] {
        let anonymous = app.post(&path(id, action)).json(&body).send().await;
        assert_eq!(anonymous.status(), 401);
        // Flat errors, as the runner expects.
        assert!(anonymous.json()["error"].is_string());
        let forged = app
            .post(&path(id, action))
            .bearer("nope")
            .json(&body)
            .send()
            .await;
        assert_eq!(forged.status(), 401);
    }
    let admin = app.create_user("admin@example.test", true, "active").await;
    let key = app.create_api_key(admin.id).await;
    assert_eq!(
        app.post(&path(id, "claim"))
            .bearer(&key)
            .json(&body)
            .send()
            .await
            .status(),
        401
    );
    let principal = app.create_principal("slack_channel", "c", admin.id).await;
    let proxy = app.create_proxy("sbx", principal).await;
    let sandbox = app.sandbox_token(proxy, principal).await;
    assert_eq!(
        app.post(&path(id, "claim"))
            .bearer(&sandbox)
            .json(&body)
            .send()
            .await
            .status(),
        401
    );
    // Nothing moved.
    assert_eq!(row(&app, id).await.last_enqueued_at, None);
}

#[tokio::test]
async fn a_claim_advances_the_schedule_and_returns_the_run_input() {
    let Some(app) = app().await else {
        return;
    };
    let (id, due) = setup(&app).await;
    let response = post(
        &app,
        &path(id, "claim"),
        &json!({"scheduled_for": wire(due)}),
    )
    .await;
    assert_eq!(response.status(), 200, "{}", response.text());
    let body = response.json();
    assert_eq!(body["outcome"], "claimed");
    assert_eq!(body["reason"], Value::Null);
    assert_eq!(body["enabled"], true);
    assert_eq!(
        body["api_input"],
        json!({
            "prompt": "Summarize.",
            "principal": centaur_console_api::principals::provision::console_user_foreign_id("ada@example.test"),
            "channel": CHANNEL,
            "slack_user_id": SLACK_USER,
            "scheduled_task_id": task_oid(id),
            "scheduled_task_name": "Hourly",
        })
    );
    // The next occurrence after now (the clock is past the due time).
    let next = DateTime::parse_from_rfc3339(body["next_run_at"].as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    assert!(next > app.now() && next <= app.now() + Duration::hours(1));
    let stored = row(&app, id).await;
    assert_eq!(stored.last_enqueued_at, Some(due.naive_utc()));
    assert_eq!(stored.next_run_at, Some(next.naive_utc()));
    let principals: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM principals WHERE kind = 'console_user' AND console_user_id IS NOT NULL",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(principals, 1);

    // A replay of the same claim answers the same and changes nothing more.
    let replay = post(
        &app,
        &path(id, "claim"),
        &json!({"scheduled_for": wire(due)}),
    )
    .await;
    assert_eq!(replay.json()["outcome"], "claimed");
    assert_eq!(replay.json()["next_run_at"], body["next_run_at"]);
    let after = row(&app, id).await;
    assert_eq!(
        (after.next_run_at, after.last_enqueued_at),
        (stored.next_run_at, stored.last_enqueued_at)
    );
}

/// A console clock slightly behind the claimed time must still move past it.
#[tokio::test]
async fn a_claim_never_hands_the_same_occurrence_out_again() {
    let Some(app) = app().await else {
        return;
    };
    let author = app.create_user("ada@example.test", false, "active").await;
    slack_identity(&app, author.id, SLACK_USER, TEAM).await;
    bot_channel(&app, CHANNEL, TEAM, "incidents", false, &[]).await;
    // The console's clock is still before the occurrence it is claiming (the
    // hour after now), so counting from now alone would find it again.
    let due = chrono::DurationRound::duration_trunc(app.now(), Duration::hours(1)).unwrap()
        + Duration::hours(1);
    let id = due_task(&app, author.id, due).await;
    let response = post(
        &app,
        &path(id, "claim"),
        &json!({"scheduled_for": wire(due)}),
    )
    .await;
    assert_eq!(response.json()["outcome"], "claimed", "{}", response.text());
    let next = DateTime::parse_from_rfc3339(response.json()["next_run_at"].as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    assert!(next > due, "{next} should follow {due}");
    assert_eq!(row(&app, id).await.next_run_at, Some(next.naive_utc()));
}

#[tokio::test]
async fn claims_that_do_not_apply_change_nothing() {
    let Some(app) = app().await else {
        return;
    };
    let (id, due) = setup(&app).await;
    let before = row(&app, id).await;

    let wrong = post(
        &app,
        &path(id, "claim"),
        &json!({"scheduled_for": wire(due + Duration::hours(1))}),
    )
    .await;
    assert_eq!(
        (
            wrong.json()["outcome"].as_str(),
            wrong.json()["reason"].as_str()
        ),
        (Some("skipped"), Some("not_due"))
    );

    // A manual claim leaves the schedule alone.
    let manual = post(
        &app,
        &path(id, "claim"),
        &json!({"scheduled_for": wire(app.now()), "manual": true}),
    )
    .await;
    assert_eq!(manual.json()["outcome"], "claimed");
    let after = row(&app, id).await;
    assert_eq!(
        (after.next_run_at, after.last_enqueued_at),
        (before.next_run_at, before.last_enqueued_at)
    );

    sqlx::query("UPDATE scheduled_tasks SET enabled = FALSE WHERE id = $1")
        .bind(id)
        .execute(&app.db.pool)
        .await
        .unwrap();
    let disabled = post(
        &app,
        &path(id, "claim"),
        &json!({"scheduled_for": wire(due)}),
    )
    .await;
    assert_eq!(
        (
            disabled.json()["outcome"].as_str(),
            disabled.json()["reason"].as_str(),
            disabled.json()["enabled"].as_bool()
        ),
        (Some("skipped"), Some("task_disabled"), Some(false))
    );

    let unknown = post(
        &app,
        &path(id + 100, "claim"),
        &json!({"scheduled_for": wire(due)}),
    )
    .await;
    assert_eq!(unknown.status(), 404);
    assert_eq!(unknown.json()["error"], "scheduled task not found");
    let invalid = post(
        &app,
        &path(id, "claim"),
        &json!({"scheduled_for": "yesterday"}),
    )
    .await;
    assert_eq!(invalid.status(), 422);
    let missing = post(&app, &path(id, "claim"), &json!({})).await;
    assert_eq!(missing.status(), 422);
}

#[tokio::test]
async fn a_claim_that_cannot_build_its_input_records_why_and_keeps_the_schedule() {
    let Some(app) = app().await else {
        return;
    };
    let (id, due) = setup(&app).await;
    // The channel is gone from the bot's catalog.
    sqlx::query("DELETE FROM slack_bot_channels")
        .execute(&app.db.pool)
        .await
        .unwrap();
    let response = post(
        &app,
        &path(id, "claim"),
        &json!({"scheduled_for": wire(due)}),
    )
    .await;
    let body = response.json();
    assert_eq!(body["outcome"], "failed");
    assert_eq!(body["reason"], "delivery_destination_unavailable");
    assert_eq!(
        body["error"],
        "Slack delivery destination is no longer available to the task owner"
    );
    let stored = row(&app, id).await;
    assert_eq!(stored.last_error.as_deref(), body["error"].as_str());
    assert_eq!(stored.last_enqueued_at, Some(due.naive_utc()));
    assert!(stored.next_run_at.is_some_and(|next| next.and_utc() > due));
}

#[tokio::test]
async fn results_write_only_the_run_bookkeeping() {
    let Some(app) = app().await else {
        return;
    };
    let (id, due) = setup(&app).await;
    let claimed = post(
        &app,
        &path(id, "claim"),
        &json!({"scheduled_for": wire(due)}),
    )
    .await;
    assert_eq!(claimed.json()["outcome"], "claimed");
    let before = row(&app, id).await;

    let reported = post(
        &app,
        &path(id, "result"),
        &json!({"scheduled_for": wire(due), "run_id": "run-42"}),
    )
    .await;
    assert_eq!(reported.status(), 200, "{}", reported.text());
    assert_eq!(reported.json()["enabled"], true);
    assert_eq!(
        reported.json()["next_run_at"],
        claimed.json()["next_run_at"]
    );
    let after = row(&app, id).await;
    assert_eq!(after.last_run_id.as_deref(), Some("run-42"));
    assert_eq!(after.last_run_at, Some(app.now().naive_utc()));
    assert_eq!(after.last_error, None);
    assert_eq!(
        (
            after.next_run_at,
            after.last_enqueued_at,
            after.name.as_str()
        ),
        (before.next_run_at, before.last_enqueued_at, "Hourly")
    );

    let failed = post(
        &app,
        &path(id, "result"),
        &json!({"scheduled_for": wire(due), "error": "boom"}),
    )
    .await;
    assert_eq!(failed.status(), 200);
    let after = row(&app, id).await;
    assert_eq!(after.last_error.as_deref(), Some("boom"));
    assert_eq!(after.last_run_id.as_deref(), Some("run-42"));

    for body in [
        json!({"scheduled_for": wire(due)}),
        json!({"scheduled_for": wire(due), "run_id": "r", "error": "e"}),
    ] {
        assert_eq!(post(&app, &path(id, "result"), &body).await.status(), 422);
    }
    let unknown = post(
        &app,
        &path(id + 1, "result"),
        &json!({"scheduled_for": wire(due), "run_id": "r"}),
    )
    .await;
    assert_eq!(unknown.status(), 404);
}
