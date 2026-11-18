//! `/api/admin/scheduled-tasks/*`: the console's reconcile and run-now calls
//! into the scheduled-task runner (RFC 0006a "api-rs endpoints").

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use centaur_scheduled_tasks::{OID_PREFIX, RunnerError, ScheduledTaskRunner, keys};
use serde_json::{Value, json};

use crate::{ApiError, routes::AppState};

fn runner(state: &AppState) -> Result<ScheduledTaskRunner, ApiError> {
    state.scheduled_task_runner().ok_or_else(|| {
        ApiError::ServiceUnavailable("scheduled task runner is not enabled".to_owned())
    })
}

fn task_id(oid: &str) -> Result<i64, ApiError> {
    centaur_console_db::decode_oid(OID_PREFIX, oid)
        .ok_or_else(|| ApiError::NotFound("scheduled task not found".to_owned()))
}

fn unavailable(error: RunnerError) -> ApiError {
    tracing::error!(%error, "scheduled task runner request failed");
    ApiError::ServiceUnavailable("scheduled task runner is unavailable".to_owned())
}

pub(crate) async fn reconcile_scheduled_task(
    State(state): State<AppState>,
    Path(oid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let runner = runner(&state)?;
    let id = task_id(&oid)?;
    let outcome = runner.reconcile_task(id).await.map_err(unavailable)?;
    Ok(Json(json!({
        "ok": true,
        "scheduled_task_id": oid,
        "state": outcome.state,
        "next_run_at": outcome.next_run_at.map(keys::wire_time),
        "tick": outcome.tick,
        "cancelled": outcome.cancelled,
    })))
}

pub(crate) async fn run_scheduled_task(
    State(state): State<AppState>,
    Path(oid): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let runner = runner(&state)?;
    let id = task_id(&oid)?;
    let queued = runner
        .run_now(id, chrono::Utc::now())
        .await
        .map_err(unavailable)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "ok": true,
            "queued": true,
            "scheduled_task_id": oid,
            "scheduled_for": keys::wire_time(queued.scheduled_for),
            "tick": { "task_id": queued.task_id, "created": queued.created },
        })),
    ))
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};

    use async_trait::async_trait;
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request, StatusCode, header},
    };
    use centaur_scheduled_tasks::spawner::RunRequest;
    use centaur_scheduled_tasks::{
        ConsoleClient, RunSpawner, RunnerConfig, ScheduledTaskRunner, SpawnError, SpawnedRun,
    };
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde_json::{Value, json};
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
    use tower::ServiceExt;

    use crate::{ApiAuthConfig, AppState, build_router_with_app_state};

    fn token(claims: Value) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(b"test-secret"),
        )
        .unwrap()
    }

    fn console_token() -> String {
        token(json!({
            "iss": "centaur-console", "sub": "centaur-console", "aud": "centaur-api",
            "iat": 1_700_000_000i64, "exp": 4_102_444_800i64, "token_use": "console_service",
        }))
    }

    fn principal_token() -> String {
        token(json!({
            "iss": "centaur-console", "sub": "prn_5CO4fITZ", "aud": "centaur-api",
            "iat": 1_700_000_000i64, "exp": 4_102_444_800i64,
            "capabilities": {"sessions_read": true, "workflows_read": true, "workflows_write": true},
        }))
    }

    async fn post(state: &AppState, path: &str, bearer: Option<&str>) -> (StatusCode, Value) {
        let mut request = Request::builder().method(Method::POST).uri(path);
        if let Some(bearer) = bearer {
            request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
        }
        let response = build_router_with_app_state(state.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn routes_require_the_console_service_jwt() {
        let state = AppState::unready(ApiAuthConfig::testing("test-secret"));
        let oid = centaur_console_db::oid("tsk", 1);
        for action in ["reconcile", "run"] {
            let path = format!("/api/admin/scheduled-tasks/{oid}/{action}");
            assert_eq!(post(&state, &path, None).await.0, StatusCode::UNAUTHORIZED);
            assert_eq!(
                post(&state, &path, Some(&principal_token())).await.0,
                StatusCode::FORBIDDEN
            );
            let (status, body) = post(&state, &path, Some(&console_token())).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(body["ok"], json!(false));
        }
    }

    struct NoSpawner;

    #[async_trait]
    impl RunSpawner for NoSpawner {
        async fn spawn_run(&self, _: RunRequest) -> Result<SpawnedRun, SpawnError> {
            Err(SpawnError::new("not in this test"))
        }
    }

    #[tokio::test]
    async fn reconcile_and_run_answer_the_documented_shapes() {
        let Ok(url) = std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL") else {
            eprintln!("skipping: SESSION_RUNTIME_TEST_DATABASE_URL not set");
            return;
        };
        let store = centaur_session_sqlx::PgSessionStore::connect(&url)
            .await
            .unwrap();
        store
            .run_migrations(centaur_session_sqlx::TextSearchBackend::Postgres)
            .await
            .unwrap();
        let unique = uuid::Uuid::new_v4().simple().to_string();
        let schema = format!("console_{unique}");
        sqlx::raw_sql(&format!(
            "create schema {schema};
             create table {schema}.scheduled_tasks (
               id bigserial primary key, enabled boolean not null default true,
               next_run_at timestamp(6), last_enqueued_at timestamp(6));
             insert into {schema}.scheduled_tasks (next_run_at) values ('2099-01-01 09:00:00');"
        ))
        .execute(store.pool())
        .await
        .unwrap();
        let console_db = PgPoolOptions::new()
            .connect_with(
                PgConnectOptions::from_str(&url)
                    .unwrap()
                    .options([("search_path", schema.as_str())]),
            )
            .await
            .unwrap();
        let runner = ScheduledTaskRunner::new(
            RunnerConfig {
                queue_name: format!("st_{unique}"),
                reconcile_interval: None,
                ..RunnerConfig::default()
            },
            store.pool().clone(),
            console_db.clone(),
            ConsoleClient::new(
                "http://127.0.0.1:9",
                "test-secret",
                std::time::Duration::from_secs(1),
            )
            .unwrap(),
            Arc::new(NoSpawner),
        )
        .await
        .unwrap();
        let handle = runner.start("test");
        let state = AppState::unready(ApiAuthConfig::testing("test-secret"));
        state.set_scheduled_task_runner(runner, handle);
        let id: i64 = sqlx::query_scalar("select id from scheduled_tasks")
            .fetch_one(&console_db)
            .await
            .unwrap();
        let oid = centaur_console_db::oid("tsk", id);

        let path = format!("/api/admin/scheduled-tasks/{oid}/reconcile");
        let (status, body) = post(&state, &path, Some(&console_token())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["scheduled_task_id"], json!(oid));
        assert_eq!(body["state"], json!("scheduled"));
        assert_eq!(body["next_run_at"], json!("2099-01-01T09:00:00Z"));
        assert_eq!(body["cancelled"], json!(0));
        // The startup reconcile may have created it already.
        assert!(["created", "existing"].contains(&body["tick"]["action"].as_str().unwrap()));

        let (status, body) = post(
            &state,
            &format!("/api/admin/scheduled-tasks/{oid}/run"),
            Some(&console_token()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["queued"], json!(true));
        assert_eq!(body["tick"]["created"], json!(true));
        assert!(body["scheduled_for"].as_str().unwrap().ends_with('Z'));

        sqlx::query("delete from scheduled_tasks")
            .execute(&console_db)
            .await
            .unwrap();
        let (status, body) = post(&state, &path, Some(&console_token())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["state"], json!("deleted"));
        assert_eq!(body["tick"], Value::Null);

        let (status, _) = post(
            &state,
            "/api/admin/scheduled-tasks/tsk_nope/reconcile",
            Some(&console_token()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
