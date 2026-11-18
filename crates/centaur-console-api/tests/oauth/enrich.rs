//! Slack identity enrichment after a consent.

use centaur_console_api::oauth::enrich;
use centaur_console_db::Encryptor;
use serde_json::json;

use super::support::*;

const AUTH_TEST: &str = "slack.com/api/auth.test";
const USERS_INFO: &str = "slack.com/api/users.info";

struct Slack {
    credential_id: i64,
    secret_id: i64,
}

/// A Slack credential as the consent left it, with its wrapper.
async fn slack_credential(f: &Fixture, scopes: &[&str]) -> Slack {
    f.insert_app("slack", "slack", false).await;
    let now = f.app.now().naive_utc();
    let token = Encryptor::development().encrypt("OLD-ACCESS").unwrap();
    let credential_id: i64 = sqlx::query_scalar(
        "INSERT INTO broker_credentials (oauth_app_id, provider_subject, token_endpoint, name, scopes, \
         access_token, created_by_id, created_at, updated_at) \
         VALUES ((SELECT id FROM oauth_apps WHERE slug = 'slack'), 'U1', 'https://slack.com/api/oauth.v2.access', \
         'Slack – U1', $1, $2, $3, $4, $4) RETURNING id",
    )
    .bind(json!(scopes))
    .bind(token)
    .bind(f.member.id)
    .bind(now)
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    let secret_id: i64 = sqlx::query_scalar(
        "INSERT INTO static_secrets (name, kind, labels, broker_credential_id, created_at, updated_at) \
         VALUES ('Slack – U1 token', 'custom', '{}', $1, $2, $2) RETURNING id",
    )
    .bind(credential_id)
    .bind(now)
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO secret_sources (source_type, broker_credential_id, static_secret_id, config, created_at, updated_at) \
         VALUES ('token_broker', $1, $2, '{}', $3, $3)",
    )
    .bind(credential_id)
    .bind(secret_id)
    .bind(now)
    .execute(&f.app.db.pool)
    .await
    .unwrap();
    Slack {
        credential_id,
        secret_id,
    }
}

async fn names(f: &Fixture) -> (String, String, Option<String>) {
    let (name, email): (String, Option<String>) =
        sqlx::query_as("SELECT name, provider_email FROM broker_credentials")
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    let secret: String = sqlx::query_scalar("SELECT name FROM static_secrets")
        .fetch_one(&f.app.db.pool)
        .await
        .unwrap();
    (name, secret, email)
}

fn answer_slack(f: &Fixture) {
    f.stub
        .answer(AUTH_TEST, 200, &json!({"ok": true, "user": "ada"}));
    f.stub.answer(
        USERS_INFO,
        200,
        &json!({"ok": true, "user": {"name": "ada", "profile": {"display_name": "Ada L", "email": "ada@example.test"}}}),
    );
}

#[tokio::test]
async fn names_the_credential_and_its_wrapper_after_the_slack_profile() {
    let Some(f) = fixture().await else { return };
    let slack = slack_credential(&f, &["search:read", "users:read", "users:read.email"]).await;
    answer_slack(&f);
    let principal = grant(&f, slack.secret_id).await;
    let before = f.version(principal).await;
    enrich::run(&f.app.state, slack.credential_id)
        .await
        .unwrap();
    assert_eq!(
        names(&f).await,
        (
            "Slack – Ada L".to_owned(),
            "Slack – Ada L token".to_owned(),
            Some("ada@example.test".to_owned())
        )
    );
    assert_eq!(
        f.version(principal).await,
        before + 1,
        "the wrapper's name is part of the principal's config"
    );
    let request = &f.stub.requests_to(USERS_INFO)[0];
    assert_eq!(request.authorization.as_deref(), Some("Bearer OLD-ACCESS"));
    assert_eq!(request.form_value("user").as_deref(), Some("U1"));
}

#[tokio::test]
async fn without_the_read_scopes_only_the_token_owner_name_is_known() {
    let Some(f) = fixture().await else { return };
    let slack = slack_credential(&f, &["search:read"]).await;
    answer_slack(&f);
    enrich::run(&f.app.state, slack.credential_id)
        .await
        .unwrap();
    assert_eq!(
        names(&f).await,
        (
            "Slack – ada".to_owned(),
            "Slack – ada token".to_owned(),
            None
        )
    );
    assert!(f.stub.requests_to(USERS_INFO).is_empty());
}

#[tokio::test]
async fn an_operators_wrapper_name_is_kept() {
    let Some(f) = fixture().await else { return };
    let slack = slack_credential(&f, &["users:read"]).await;
    sqlx::query("UPDATE static_secrets SET name = 'Operator name'")
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    answer_slack(&f);
    enrich::run(&f.app.state, slack.credential_id)
        .await
        .unwrap();
    assert_eq!(
        names(&f).await,
        ("Slack – Ada L".to_owned(), "Operator name".to_owned(), None)
    );
}

#[tokio::test]
async fn leaves_the_credential_alone_when_slack_does_not_answer() {
    let Some(f) = fixture().await else { return };
    let slack = slack_credential(&f, &["users:read"]).await;
    // The stub answers 404 `{}` to everything unknown.
    enrich::run(&f.app.state, slack.credential_id)
        .await
        .unwrap();
    assert_eq!(
        names(&f).await,
        ("Slack – U1".to_owned(), "Slack – U1 token".to_owned(), None)
    );
    // Other providers' credentials are never touched, nor unknown ids.
    sqlx::query("UPDATE oauth_apps SET provider = 'google'")
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    answer_slack(&f);
    enrich::run(&f.app.state, slack.credential_id)
        .await
        .unwrap();
    enrich::run(&f.app.state, slack.credential_id + 1000)
        .await
        .unwrap();
    assert_eq!(names(&f).await.0, "Slack – U1");
}

/// Regression (review finding 66a99e): enrichment awaits Slack, and a token
/// refresh landing meanwhile must survive: only `name`, `provider_email`, and
/// `updated_at` of the credential are written.
#[tokio::test]
async fn a_token_refresh_during_the_slack_calls_is_not_undone() {
    let Some(f) = fixture().await else { return };
    let slack = slack_credential(&f, &["users:read", "users:read.email"]).await;
    answer_slack(&f);
    let db = f.app.db.pool.clone();
    let refreshed = Encryptor::development()
        .encrypt("REFRESHED-ACCESS")
        .unwrap();
    f.stub.meanwhile(USERS_INFO, move || {
        let (db, refreshed) = (db.clone(), refreshed.clone());
        Box::pin(async move {
            // What token-broker writes after a refresh (or after it declared
            // the credential dead).
            sqlx::query(
                "UPDATE broker_credentials SET access_token = $1, refresh_token = $1, dead = TRUE, \
                 dead_reason = 'invalid_grant', failure_count = 7, expires_at = now() + interval '2 hours', \
                 last_refresh = now(), next_attempt_at = now() + interval '1 hour'",
            )
            .bind(refreshed)
            .execute(&db)
            .await
            .unwrap();
        })
    });
    enrich::run(&f.app.state, slack.credential_id)
        .await
        .unwrap();

    let row: (String, Option<String>, bool, Option<String>, i32, String) = sqlx::query_as(
        "SELECT access_token, refresh_token, dead, dead_reason, failure_count, name FROM broker_credentials",
    )
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    let refreshed = Encryptor::development()
        .decrypt(&centaur_console_db::Ciphertext::from_stored(row.0))
        .unwrap();
    assert_eq!(
        refreshed, "REFRESHED-ACCESS",
        "the refreshed token was restored to the stale one"
    );
    assert!(row.1.is_some());
    assert_eq!(
        (row.2, row.3.as_deref(), row.4),
        (true, Some("invalid_grant"), 7),
        "dead state was cleared"
    );
    assert_eq!(row.5, "Slack – Ada L");
}
