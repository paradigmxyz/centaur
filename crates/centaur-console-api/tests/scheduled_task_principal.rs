//! A scheduled-task claim provisions the author's console-user principal
//! through the principals area, and the principal's commit callbacks (OAuth
//! credential reconciliation) run: the author's minted credential ends up
//! granted to the principal the run executes as.

mod automation_support;

use automation_support::*;
use centaur_console_api::{automation, testing::TestApp};
use serde_json::json;

#[tokio::test]
async fn a_claim_that_provisions_the_author_runs_the_principal_commit_hooks() {
    let Some(app) = TestApp::new(automation::routes(false)).await else {
        return;
    };
    let author = app.create_user("ada@example.test", false, "active").await;
    slack_identity(&app, author.id, SLACK_USER, TEAM).await;
    bot_channel(&app, CHANNEL, TEAM, "incidents", false, &[]).await;
    let now = app.now().naive_utc();
    let app_id: i64 = sqlx::query_scalar(
        "INSERT INTO oauth_apps (slug, provider, client_id, allowed_scopes, enabled, created_by_id, \
         created_at, updated_at) VALUES ('google', 'google', 'cid', '[\"x\"]', TRUE, $1, $2, $2) RETURNING id",
    )
    .bind(author.id)
    .bind(now)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    let credential: i64 = sqlx::query_scalar(
        "INSERT INTO broker_credentials (oauth_app_id, provider_subject, token_endpoint, created_by_id, \
         created_at, updated_at) VALUES ($1, 'sub-1', 'https://oauth2.googleapis.com/token', $2, $3, $3) \
         RETURNING id",
    )
    .bind(app_id)
    .bind(author.id)
    .bind(now)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    let secret_id: i64 = sqlx::query_scalar(
        "INSERT INTO static_secrets (name, kind, labels, broker_credential_id, inject_config, \
         created_at, updated_at) VALUES ('Google token', 'custom', '{}', $1, '{\"header\":\"Authorization\"}', $2, $2) \
         RETURNING id",
    )
    .bind(credential)
    .bind(now)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();

    let id = insert_task(&app, author.id, "Digest", Some("0 * * * *")).await;
    let due: chrono::NaiveDateTime =
        sqlx::query_scalar("SELECT next_run_at FROM scheduled_tasks WHERE id = $1")
            .bind(id)
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    let path = format!("/internal/v1/scheduled_tasks/{}/claim", task_oid(id));
    let body = json!({"scheduled_for": due.and_utc().to_rfc3339()});
    let claim = || {
        app.post(&path)
            .bearer(&app.internal_token())
            .json(&body)
            .send()
    };

    let response = claim().await;
    assert_eq!(response.status(), 200, "{}", response.text());
    assert_eq!(response.json()["outcome"], "claimed", "{}", response.text());
    let (principal, foreign_id): (i64, Option<String>) = sqlx::query_as(
        "SELECT id, foreign_id FROM principals WHERE kind = 'console_user' AND console_user_id = $1",
    )
    .bind(author.id)
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(response.json()["api_input"]["principal"], json!(foreign_id));

    let grants = || async {
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT principal_id, static_secret_id FROM grants WHERE static_secret_id IS NOT NULL ORDER BY id",
        )
        .fetch_all(&app.db.pool)
        .await
        .unwrap()
    };
    assert_eq!(grants().await, [(principal, secret_id)]);

    // A replay finds the principal unchanged: no duplicate grant.
    let replay = claim().await;
    assert_eq!(replay.json()["outcome"], "claimed");
    assert_eq!(grants().await, [(principal, secret_id)]);
}
