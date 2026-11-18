//! Shared fixtures for the automation tests: a stub api-rs and rows.

#![allow(dead_code)]

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU16, Ordering},
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
};
use centaur_console_api::testing::TestApp;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Call {
    pub method: String,
    pub path: String,
    pub bearer: bool,
    pub body: Value,
}

/// A stand-in for api-rs.
pub struct Api {
    pub calls: Mutex<Vec<Call>>,
    /// The status of `POST /api/admin/scheduled-tasks/*/run`.
    pub run_status: AtomicU16,
    /// Every request answers 503.
    pub down: AtomicBool,
    pub teams: Mutex<Vec<String>>,
    pub schedules: Mutex<Value>,
}

impl Api {
    pub fn calls_to(&self, suffix: &str) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.path.ends_with(suffix))
            .cloned()
            .collect()
    }
}

async fn answer(
    State(api): State<Arc<Api>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let path = uri.path().to_owned();
    api.calls.lock().unwrap().push(Call {
        method: method.to_string(),
        path: path.clone(),
        bearer: headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("Bearer ")),
        body: body.clone(),
    });
    if api.down.load(Ordering::SeqCst) {
        return (StatusCode::SERVICE_UNAVAILABLE, "down").into_response();
    }
    let json_ok = |value: Value| axum::Json(value).into_response();
    match (method.as_str(), path.as_str()) {
        ("POST", p) if p.ends_with("/run") => (
            StatusCode::from_u16(api.run_status.load(Ordering::SeqCst)).unwrap(),
            "{}",
        )
            .into_response(),
        ("POST", p) if p.ends_with("/reconcile") => json_ok(json!({})),
        ("GET", "/api/slack/external-team-ids") => {
            json_ok(json!({"slack_external_team_ids": *api.teams.lock().unwrap()}))
        }
        ("PUT", "/api/slack/external-team-ids") => {
            let ids: Vec<String> = body["slack_external_team_ids"]
                .as_array()
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| id.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if ids.iter().any(|id| id == "bad") {
                return (
                    StatusCode::BAD_REQUEST,
                    axum::Json(json!({"error": "invalid team id"})),
                )
                    .into_response();
            }
            *api.teams.lock().unwrap() = ids.clone();
            json_ok(json!({"slack_external_team_ids": ids}))
        }
        ("GET", "/api/workflows/schedules") => {
            json_ok(json!({"schedules": *api.schedules.lock().unwrap()}))
        }
        ("GET", p) if p.starts_with("/api/workflows/runs/") => json_ok(json!({"run": {
            "run_id": p.trim_start_matches("/api/workflows/runs/"),
            "input": {"prompt": "hello"},
            "failure": null,
        }})),
        ("POST", "/api/workflows/runs") => {
            json_ok(json!({"run_id": "run-1", "created": body["workflow_name"] != "existing"}))
        }
        _ => (StatusCode::NOT_FOUND, "{}").into_response(),
    }
}

pub async fn stub_api() -> (Arc<Api>, String) {
    let api = Arc::new(Api {
        calls: Mutex::default(),
        run_status: AtomicU16::new(202),
        down: AtomicBool::new(false),
        teams: Mutex::default(),
        schedules: Mutex::new(json!([])),
    });
    let router = Router::new().fallback(answer).with_state(api.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (api, base)
}

/// A Slack sign-in identity for the user.
pub async fn slack_identity(app: &TestApp, user_id: i64, slack_user: &str, team: &str) {
    sqlx::query(
        "INSERT INTO user_identities (user_id, provider, subject, team_id, created_at, updated_at) \
         VALUES ($1, 'slack', $2, $3, now(), now())",
    )
    .bind(user_id)
    .bind(slack_user)
    .bind(team)
    .execute(&app.db.pool)
    .await
    .unwrap();
}

/// A channel the bot is in.
pub async fn bot_channel(
    app: &TestApp,
    channel_id: &str,
    team: &str,
    name: &str,
    private: bool,
    members: &[&str],
) {
    let members: Vec<String> = members.iter().map(|member| (*member).to_owned()).collect();
    sqlx::query(
        "INSERT INTO slack_bot_channels (channel_id, team_id, name, private, bot_user_id, \
         member_user_ids, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'UBOT00000', $5, now(), now())",
    )
    .bind(channel_id)
    .bind(team)
    .bind(name)
    .bind(private)
    .bind(members)
    .execute(&app.db.pool)
    .await
    .unwrap();
}

pub const TEAM: &str = "T0123456789";
pub const SLACK_USER: &str = "U0123456789";
pub const CHANNEL: &str = "C0123456789";

/// A task row; returns its id.
pub async fn insert_task(app: &TestApp, author_id: i64, name: &str, cron: Option<&str>) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO scheduled_tasks (author_id, name, prompt, delivery_channel, cron_expression, \
         timezone, enabled, next_run_at, created_at, updated_at) \
         VALUES ($1, $2, 'Summarize.', $3, $4, 'Etc/UTC', TRUE, $5, $6, $6) RETURNING id",
    )
    .bind(author_id)
    .bind(name)
    .bind(CHANNEL)
    .bind(cron)
    .bind((app.now() + chrono::Duration::hours(1)).naive_utc())
    .bind(app.now().naive_utc())
    .fetch_one(&app.db.pool)
    .await
    .unwrap()
}

pub fn task_oid(id: i64) -> String {
    centaur_console_db::oid("tsk", id)
}
