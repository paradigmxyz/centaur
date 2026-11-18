//! The Slack channel catalog (against a stub Slack API) and the console's
//! channel picker.

mod misc_common;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use centaur_console_api::{
    auth::Access,
    misc::{self, slack::Catalog},
    testing::TestApp,
};
use centaur_console_db::oid;
use misc_common::{browser, console};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Slack {
    /// `conversations.members` answers per channel; a `Value::Null` entry
    /// is a 429 once.
    members: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    channels: Arc<Mutex<Vec<Value>>>,
    calls: Arc<Mutex<Vec<String>>>,
}

async fn method(
    State(slack): State<Slack>,
    Path(method): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: axum::http::HeaderMap,
) -> Response {
    assert_eq!(
        headers.get("authorization").and_then(|v| v.to_str().ok()),
        Some("Bearer xoxb-test-token")
    );
    slack.calls.lock().unwrap().push(method.clone());
    match method.as_str() {
        "auth.test" => {
            Json(json!({"ok": true, "team_id": "T0000001", "user_id": "U0BOT0001"})).into_response()
        }
        "conversations.list" => {
            let channels = slack.channels.lock().unwrap().clone();
            // Two pages: the first holds the first two channels.
            match query.get("cursor").map(String::as_str) {
                None | Some("") => Json(
                    json!({"ok": true, "channels": channels[..2.min(channels.len())],
                    "response_metadata": {"next_cursor": "page2"}}),
                )
                .into_response(),
                _ => Json(
                    json!({"ok": true, "channels": channels[2.min(channels.len())..],
                    "response_metadata": {"next_cursor": ""}}),
                )
                .into_response(),
            }
        }
        "conversations.info" => {
            let id = query["channel"].clone();
            let found = slack
                .channels
                .lock()
                .unwrap()
                .iter()
                .find(|c| c["id"] == id)
                .cloned();
            match found {
                Some(channel) => Json(json!({"ok": true, "channel": channel})).into_response(),
                None => Json(json!({"ok": false, "error": "channel_not_found"})).into_response(),
            }
        }
        "conversations.members" => {
            let id = query["channel"].clone();
            let mut members = slack.members.lock().unwrap();
            let Some(answers) = members.get_mut(&id) else {
                return Json(json!({"ok": false, "error": "missing_scope"})).into_response();
            };
            if answers.first() == Some(&Value::Null) {
                answers.remove(0);
                return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "0.01")], "")
                    .into_response();
            }
            Json(json!({"ok": true, "members": answers.clone()})).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "").into_response(),
    }
}

async fn serve() -> (Slack, String) {
    let slack = Slack::default();
    let router = Router::new()
        .route("/{method}", get(method))
        .with_state(slack.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (slack, base)
}

fn channel(id: &str, name: &str, private: bool, archived: bool) -> Value {
    json!({"id": id, "name": name, "is_private": private, "is_archived": archived})
}

async fn app_with_slack() -> Option<(TestApp, Slack)> {
    let (slack, base) = serve().await;
    let app = TestApp::with_env(
        misc::routes(),
        &[
            ("CENTAUR_CONSOLE_SLACK_BOT_TOKEN", "xoxb-test-token"),
            ("SLACK_API_URL", &base),
        ],
    )
    .await?;
    Some((app, slack))
}

#[derive(sqlx::FromRow, Debug, PartialEq)]
struct Row {
    channel_id: String,
    name: String,
    private: bool,
    archived: bool,
    active: bool,
    member_user_ids: Vec<String>,
    membership_error: Option<String>,
}

async fn rows(app: &TestApp) -> Vec<Row> {
    sqlx::query_as(
        "SELECT channel_id, name, private, archived, active, member_user_ids, membership_error \
         FROM slack_bot_channels ORDER BY channel_id",
    )
    .fetch_all(&app.db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn discovery_replaces_the_catalog_and_deactivates_vanished_channels() {
    let Some((app, slack)) = app_with_slack().await else {
        return;
    };
    *slack.channels.lock().unwrap() = vec![
        channel("C1", "general", false, false),
        channel("G2", "secret", true, false),
        json!({"id": "D3", "user": "U1", "is_im": true}),
        channel("C4", "old", false, true),
    ];
    let catalog = Catalog::from_state(&app.state);
    catalog.refresh().await.unwrap();
    let found = rows(&app).await;
    let summary: Vec<_> = found
        .iter()
        .map(|r| (r.channel_id.as_str(), r.private, r.archived, r.active))
        .collect();
    assert_eq!(
        summary,
        [
            ("C1", false, false, true),
            ("C4", false, true, false),
            ("G2", true, false, true)
        ]
    );
    let team: String =
        sqlx::query_scalar("SELECT DISTINCT team_id || '/' || bot_user_id FROM slack_bot_channels")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(team, "T0000001/U0BOT0001");
    // Remember some members, then rename and drop channels: members survive,
    // missing channels go inactive.
    sqlx::query("UPDATE slack_bot_channels SET member_user_ids = '{U1}' WHERE channel_id = 'C1'")
        .execute(&app.db.pool)
        .await
        .unwrap();
    *slack.channels.lock().unwrap() = vec![channel("C1", "lobby", false, false)];
    catalog.refresh().await.unwrap();
    let found = rows(&app).await;
    let c1 = found.iter().find(|r| r.channel_id == "C1").unwrap();
    assert_eq!(
        (c1.name.as_str(), c1.active, c1.member_user_ids.clone()),
        ("lobby", true, vec!["U1".to_owned()])
    );
    assert!(
        found
            .iter()
            .filter(|r| r.channel_id != "C1")
            .all(|r| !r.active)
    );
}

#[tokio::test]
async fn membership_refresh_stores_members_errors_and_retries_rate_limits() {
    let Some((app, slack)) = app_with_slack().await else {
        return;
    };
    *slack.channels.lock().unwrap() = vec![
        channel("C1", "one", false, false),
        channel("C2", "two", false, false),
        channel("C3", "three", false, false),
    ];
    let catalog = Catalog::from_state(&app.state);
    catalog.refresh().await.unwrap();
    {
        let mut members = slack.members.lock().unwrap();
        // C1 is rate limited once, then answers; C2 answers unsorted with
        // duplicates; C3 has no scope.
        members.insert("C1".into(), vec![Value::Null, json!("U2"), json!("U1")]);
        members.insert("C2".into(), vec![json!("U9"), json!("U3"), json!("U9")]);
    }
    catalog.refresh_membership(None).await.unwrap();
    let found = rows(&app).await;
    assert_eq!(found[0].member_user_ids, ["U1", "U2"]);
    assert_eq!(found[1].member_user_ids, ["U3", "U9"]);
    assert!(found[2].member_user_ids.is_empty());
    assert!(
        found[2]
            .membership_error
            .as_deref()
            .unwrap()
            .contains("missing_scope")
    );
    assert!(found[0].membership_error.is_none());
    let refreshed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM slack_bot_channels WHERE membership_refreshed_at IS NOT NULL",
    )
    .fetch_one(&app.db.pool)
    .await
    .unwrap();
    assert_eq!(refreshed, 2);
    // A second pass skips channels refreshed or attempted recently.
    let before = slack.calls.lock().unwrap().len();
    catalog.refresh_membership(None).await.unwrap();
    assert_eq!(slack.calls.lock().unwrap().len(), before);
}

#[tokio::test]
async fn refreshing_one_channel_imports_it_and_deactivates_unknown_ones() {
    let Some((app, slack)) = app_with_slack().await else {
        return;
    };
    *slack.channels.lock().unwrap() = vec![
        channel("C1", "one", false, false),
        channel("C2", "two", false, false),
    ];
    slack
        .members
        .lock()
        .unwrap()
        .insert("C2".into(), vec![json!("U7")]);
    let catalog = Catalog::from_state(&app.state);
    catalog.refresh().await.unwrap();
    catalog.refresh_membership(Some("C2")).await.unwrap();
    let found = rows(&app).await;
    assert_eq!(found[1].member_user_ids, ["U7"]);
    // Slack no longer knows C1: it becomes inactive instead of failing.
    slack.channels.lock().unwrap().retain(|c| c["id"] != "C1");
    catalog.refresh_membership(Some("C1")).await.unwrap();
    assert!(!rows(&app).await[0].active);
}

#[tokio::test]
async fn nothing_runs_without_a_bot_token() {
    let Some(app) = TestApp::new(misc::routes()).await else {
        return;
    };
    let catalog = Catalog::from_state(&app.state);
    assert!(!catalog.configured());
    catalog.refresh().await.unwrap();
    catalog.refresh_membership(None).await.unwrap();
    assert!(rows(&app).await.is_empty());
}

// --- the picker ---------------------------------------------------------------------

const PICKER: &str = "/api/v1/console/slack_channels";

async fn seed(app: &TestApp) {
    for (id, name, private, active, archived) in [
        ("C1", "general", false, true, false),
        ("C2", "ops-room", true, true, false),
        ("C3", "retired", false, false, false),
        ("C4", "archive", false, true, true),
    ] {
        sqlx::query(
            "INSERT INTO slack_bot_channels (team_id, channel_id, bot_user_id, name, private, active, archived, created_at, updated_at) \
             VALUES ('T0000001', $1, 'U0BOT0001', $2, $3, $4, $5, now(), now())")
            .bind(id).bind(name).bind(private).bind(active).bind(archived)
            .execute(&app.db.pool).await.unwrap();
    }
}

#[tokio::test]
async fn the_picker_is_admin_only() {
    let Some(app) = TestApp::new(misc::routes()).await else {
        return;
    };
    app.assert_console_access(Access::Admin, Method::GET, PICKER, None)
        .await;
}

#[tokio::test]
async fn the_picker_searches_active_channels() {
    let Some(app) = TestApp::new(misc::routes()).await else {
        return;
    };
    let admin = app
        .create_user("admin@example.test", true, "active")
        .await
        .id;
    let mut b = browser(&app, admin);
    // Without a token the picker says so.
    let empty = console(&app, &mut b, Method::GET, PICKER, None).await;
    assert_eq!(empty.status(), 200);
    assert_eq!(empty.header("cache-control"), Some("no-store"));
    assert_eq!(
        empty.json()["meta"]["catalog_error"],
        "SLACK_BOT_TOKEN is not configured."
    );
    seed(&app).await;
    let all = console(&app, &mut b, Method::GET, PICKER, None).await;
    let ids: Vec<_> = all.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["channel_id"].clone())
        .collect();
    assert_eq!(ids, [json!("C1"), json!("C2")]);
    assert_eq!(
        all.json()["data"][1],
        json!({
            "channel_id": "C2", "name": "ops-room", "private": true, "label": "#ops-room",
            "description": "C2 · Private",
        })
    );
    let found = console(&app, &mut b, Method::GET, &format!("{PICKER}?q=OPS"), None).await;
    assert_eq!(found.json()["data"].as_array().unwrap().len(), 1);
    // LIKE wildcards in the query are literal.
    let wild = console(&app, &mut b, Method::GET, &format!("{PICKER}?q=%25"), None).await;
    assert_eq!(wild.json()["data"], json!([]));
    // By id, inactive channels included; bracketed arrays as the SPA sends them.
    let by_id = console(
        &app,
        &mut b,
        Method::GET,
        &format!("{PICKER}?ids[]=c3&ids[]=C1&ids[]=nope"),
        None,
    )
    .await;
    let ids: Vec<_> = by_id.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["channel_id"].clone())
        .collect();
    assert_eq!(ids, [json!("C1"), json!("C3")]);
    let dash = console(
        &app,
        &mut b,
        Method::GET,
        &format!("{PICKER}?ids[]=-"),
        None,
    )
    .await;
    assert_eq!(dash.json()["data"], json!([]));
}

#[tokio::test]
async fn the_picker_can_exclude_channels_an_owner_already_has() {
    let Some(app) = TestApp::new(misc::routes()).await else {
        return;
    };
    let admin = app
        .create_user("admin@example.test", true, "active")
        .await
        .id;
    seed(&app).await;
    let principal = app.create_principal("unknown", "p", admin).await;
    let role: i64 = sqlx::query_scalar("INSERT INTO roles (name, created_by_id, created_at, updated_at) VALUES ('r', $1, now(), now()) RETURNING id")
        .bind(admin).fetch_one(&app.db.pool).await.unwrap();
    sqlx::query("INSERT INTO slack_channel_permissions (channel_id, principal_id, created_at, updated_at) VALUES ('C1', $1, now(), now())")
        .bind(principal).execute(&app.db.pool).await.unwrap();
    sqlx::query("INSERT INTO slack_channel_permissions (channel_id, role_id, created_at, updated_at) VALUES ('C2', $1, now(), now())")
        .bind(role).execute(&app.db.pool).await.unwrap();
    let mut b = browser(&app, admin);
    let names = |response: centaur_console_api::testing::TestResponse| -> Vec<Value> {
        response.json()["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["channel_id"].clone())
            .collect()
    };
    let by_principal = console(
        &app,
        &mut b,
        Method::GET,
        &format!("{PICKER}?exclude_owner=principal:{}", oid("prn", principal)),
        None,
    )
    .await;
    assert_eq!(names(by_principal), [json!("C2")]);
    let by_role = console(
        &app,
        &mut b,
        Method::GET,
        &format!("{PICKER}?exclude_owner=role:{}", oid("role", role)),
        None,
    )
    .await;
    assert_eq!(names(by_role), [json!("C1")]);
    let unknown = console(
        &app,
        &mut b,
        Method::GET,
        &format!("{PICKER}?exclude_owner=role:{}", oid("role", 999)),
        None,
    )
    .await;
    assert_eq!(unknown.status(), 404);
    let shape = console(
        &app,
        &mut b,
        Method::GET,
        &format!("{PICKER}?exclude_owner=team:x"),
        None,
    )
    .await;
    assert_eq!(shape.status(), 400);
}

#[tokio::test]
async fn an_empty_catalog_starts_discovery_in_the_background() {
    let Some((app, slack)) = app_with_slack().await else {
        return;
    };
    *slack.channels.lock().unwrap() = vec![channel("C1", "general", false, false)];
    let admin = app
        .create_user("admin@example.test", true, "active")
        .await
        .id;
    let mut b = browser(&app, admin);
    let first = console(&app, &mut b, Method::GET, PICKER, None).await;
    assert!(
        first.json()["meta"]["catalog_error"]
            .as_str()
            .unwrap()
            .contains("loading")
    );
    for _ in 0..100 {
        if !rows(&app).await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(rows(&app).await.len(), 1);
    let second = console(&app, &mut b, Method::GET, PICKER, None).await;
    assert_eq!(second.json()["meta"]["catalog_error"], Value::Null);
    assert_eq!(second.json()["data"][0]["channel_id"], "C1");
}

/// A loop pass that dies mid-flight (shutdown `abort_all`) or panics must not
/// leave the advisory lock behind: the lock is transaction-scoped, so dropping
/// the pass's transaction releases it. Before that, the lock was session-scoped
/// on a pooled connection and stayed held forever.
#[tokio::test]
async fn a_pass_that_is_dropped_or_panics_releases_its_lock() {
    use std::{
        panic::AssertUnwindSafe,
        sync::atomic::{AtomicBool, Ordering},
    };

    use centaur_console_api::misc::slack::with_lock;
    use futures_util::FutureExt;

    let Some(app) = TestApp::new(misc::routes()).await else {
        return;
    };
    let name = "test-dropped-pass";

    // Dropped mid-pass, like `abort_all` at shutdown.
    let started = Arc::new(AtomicBool::new(false));
    let flag = started.clone();
    let pass = with_lock(&app.db.pool, name, false, move || async move {
        flag.store(true, Ordering::SeqCst);
        std::future::pending::<()>().await;
    });
    tokio::time::timeout(Duration::from_millis(200), pass)
        .await
        .expect_err("the pass never finishes");
    assert!(started.load(Ordering::SeqCst), "the pass ran");
    let again = with_lock(&app.db.pool, name, false, || async { "second" })
        .await
        .unwrap()
        .expect("the lock is free after a dropped pass");
    assert_eq!(again, "second");

    // Panicked mid-pass.
    let panicked = AssertUnwindSafe(with_lock(&app.db.pool, name, false, || async {
        panic!("the pass panicked");
    }))
    .catch_unwind()
    .await;
    assert!(panicked.is_err(), "the pass panicked");
    let again = with_lock(&app.db.pool, name, false, || async { "third" })
        .await
        .unwrap()
        .expect("the lock is free after a panicking pass");
    assert_eq!(again, "third");
}

/// A pass holds the lock's transaction and one more connection for its own
/// queries, which is what the pool's two-connection minimum provides.
#[tokio::test]
async fn a_pass_runs_on_a_two_connection_pool() {
    use centaur_console_api::misc::slack::with_lock;
    use sqlx::postgres::PgPoolOptions;

    let Some(app) = TestApp::new(misc::routes()).await else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&app.db.url)
        .await
        .expect("a two-connection pool");
    let answer = with_lock(&pool, "test-small-pool", false, || async {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&pool)
            .await
    })
    .await
    .unwrap()
    .expect("the lock is free");
    assert_eq!(answer.expect("the pass's own query"), 1);
}
