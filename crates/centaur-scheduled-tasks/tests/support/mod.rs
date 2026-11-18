//! A disposable api-rs database with the Absurd schema, a console schema
//! holding `scheduled_tasks`, a fake console implementing the RFC 0006a
//! claim/result contract over it, and a run spawner onto a private queue.

#![allow(dead_code)]

use std::{
    collections::BTreeSet,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use absurd::{Client, ClientOptions, RetryKind, RetryStrategy, SpawnOptions, WorkBatchOptions};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use centaur_scheduled_tasks::spawner::RunRequest;
use centaur_scheduled_tasks::{
    ConsoleClient, RunSpawner, RunnerConfig, ScheduledTaskRunner, SpawnError, SpawnedRun,
    console::{ClaimOutcome, ClaimRequest, ClaimResponse, ResultRequest, ResultResponse},
    keys,
};
use centaur_session_sqlx::{PgSessionStore, TextSearchBackend};
use chrono::{DateTime, NaiveDateTime, SubsecRound, Utc};
use serde_json::{Value, json};
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use tokio::sync::OnceCell;

pub const SECRET: &str = "test-jwt-signing-secret";

static MIGRATED: OnceCell<()> = OnceCell::const_new();

/// How the fake console reads its clock when computing `next_run_at`.
#[derive(Clone, Debug)]
pub enum Clock {
    Real,
    /// A claim happens exactly at `scheduled_for` (DST sequences).
    ScheduledFor,
}

impl Clock {
    fn now(&self, scheduled_for: Option<DateTime<Utc>>) -> DateTime<Utc> {
        match self {
            Clock::Real => Utc::now(),
            Clock::ScheduledFor => scheduled_for.unwrap_or_else(Utc::now),
        }
    }
}

#[derive(Default)]
pub struct Faults {
    /// Claims answered 503 before touching the row.
    pub claim_unavailable: AtomicUsize,
    /// Claims committed, then answered 500 (crash before the checkpoint).
    pub claim_commit_then_fail: AtomicUsize,
    /// Results answered 503.
    pub result_unavailable: AtomicUsize,
}

pub struct FakeConsole {
    pub pool: PgPool,
    pub clock: Mutex<Clock>,
    pub faults: Faults,
    pub claims: Mutex<Vec<(String, ClaimRequest)>>,
    pub results: Mutex<Vec<(String, ResultRequest)>>,
    /// The console clock at each result call.
    pub result_times: Mutex<Vec<DateTime<Utc>>>,
    /// Tasks whose delivery destination is no longer available.
    pub undeliverable: Mutex<BTreeSet<i64>>,
    /// Occurrences (wire `scheduled_for`) whose claim commits but answers
    /// `claimed` without the API input, which the runner treats as a hard
    /// error: such a tick never checkpoints its claim.
    pub claim_missing_api_input: Mutex<BTreeSet<String>>,
}

impl FakeConsole {
    pub fn claim_count(&self) -> usize {
        self.claims.lock().unwrap().len()
    }

    pub fn results(&self) -> Vec<ResultRequest> {
        self.results
            .lock()
            .unwrap()
            .iter()
            .map(|(_, request)| request.clone())
            .collect()
    }
}

#[derive(Default)]
pub struct SpawnFaults {
    /// Spawns that fail before creating the run.
    pub fail: AtomicUsize,
    /// Spawns that create the run and then fail (crash before checkpoint).
    pub fail_after_spawn: AtomicUsize,
}

pub struct TestSpawner {
    pub client: Client,
    pub queue: String,
    pub requests: Mutex<Vec<RunRequest>>,
    pub faults: SpawnFaults,
}

fn take(counter: &AtomicUsize) -> bool {
    counter
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
}

#[async_trait]
impl RunSpawner for TestSpawner {
    async fn spawn_run(&self, request: RunRequest) -> Result<SpawnedRun, SpawnError> {
        self.requests.lock().unwrap().push(request.clone());
        if take(&self.faults.fail) {
            return Err(SpawnError::new("internal server error"));
        }
        let spawned = spawn_workflow_run(&self.client, &self.queue, &request).await;
        if take(&self.faults.fail_after_spawn) {
            return Err(SpawnError::new("internal server error"));
        }
        Ok(SpawnedRun {
            run_id: spawned.run_id,
            created: spawned.created,
        })
    }
}

/// What `WorkflowRuntime::create_run` spawns: `centaur.workflow` with the
/// request's key and attempts.
pub async fn spawn_workflow_run(
    client: &Client,
    queue: &str,
    request: &RunRequest,
) -> absurd::SpawnResult {
    client
        .spawn(
            "centaur.workflow",
            json!({
                "workflow_name": request.workflow_name,
                "input": serde_json::to_value(&request.input).unwrap(),
                "harness_type": "codex",
            }),
            SpawnOptions {
                queue: Some(queue.to_owned()),
                idempotency_key: request.idempotency_key.clone(),
                max_attempts: request.max_attempts,
                ..SpawnOptions::default()
            },
        )
        .await
        .expect("spawn workflow run")
}

pub struct Harness {
    pub url: String,
    pub schema: String,
    pub base_url: String,
    pub pool: PgPool,
    pub console_db: PgPool,
    pub fake: Arc<FakeConsole>,
    pub spawner: Arc<TestSpawner>,
    pub runner: ScheduledTaskRunner,
    pub queue: String,
}

pub struct TaskRow {
    pub enabled: bool,
    pub next_run_at: Option<DateTime<Utc>>,
    pub last_enqueued_at: Option<DateTime<Utc>>,
    pub last_run_id: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
}

pub struct TickRow {
    pub key: String,
    pub state: String,
    pub params: Value,
    pub result: Option<Value>,
}

pub struct RunRow {
    pub key: String,
    pub task_id: String,
    pub params: Value,
    pub max_attempts: Option<i32>,
}

fn utc(value: Option<NaiveDateTime>) -> Option<DateTime<Utc>> {
    value.map(|value| value.and_utc())
}

pub fn minute(time: DateTime<Utc>) -> DateTime<Utc> {
    let time = time.trunc_subsecs(0);
    time - chrono::Duration::seconds(i64::from(chrono::Timelike::second(&time)))
}

fn no_wait(_: i32) -> Duration {
    Duration::ZERO
}

pub fn test_config(queue: &str) -> RunnerConfig {
    RunnerConfig {
        queue_name: queue.to_owned(),
        reconcile_interval: None,
        spawn_retry_delay: no_wait,
        claim_wait: Duration::ZERO,
        tick_retry: RetryStrategy {
            kind: RetryKind::Fixed,
            base_seconds: Some(0.0),
            factor: None,
            max_seconds: None,
        },
        ..RunnerConfig::default()
    }
}

impl Harness {
    /// `None` (skip) without `SESSION_RUNTIME_TEST_DATABASE_URL`.
    pub async fn new() -> Option<Self> {
        Self::with_config(|config| config).await
    }

    pub async fn with_config(edit: impl FnOnce(RunnerConfig) -> RunnerConfig) -> Option<Self> {
        Self::build(edit).await
    }

    async fn build(edit: impl FnOnce(RunnerConfig) -> RunnerConfig) -> Option<Self> {
        let Ok(url) = std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL") else {
            eprintln!("skipping: SESSION_RUNTIME_TEST_DATABASE_URL not set");
            return None;
        };
        MIGRATED
            .get_or_init(|| async {
                let store = PgSessionStore::connect(&url).await.expect("connect");
                store
                    .run_migrations(TextSearchBackend::Postgres)
                    .await
                    .expect("migrate");
            })
            .await;
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&url)
            .await
            .expect("connect api db");

        let unique = uuid::Uuid::new_v4().simple().to_string();
        let schema = format!("console_{unique}");
        sqlx::raw_sql(&format!(
            "create schema {schema};
             create table {schema}.scheduled_tasks (
               id bigserial primary key,
               name varchar not null,
               prompt text not null,
               delivery_channel varchar not null,
               cron_expression varchar,
               timezone varchar not null default 'America/Los_Angeles',
               enabled boolean not null default true,
               next_run_at timestamp(6),
               last_enqueued_at timestamp(6),
               last_run_id varchar,
               last_run_at timestamp(6),
               last_error text,
               created_at timestamp(6) not null default now(),
               updated_at timestamp(6) not null default now()
             );"
        ))
        .execute(&pool)
        .await
        .expect("create console schema");
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let console_db = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options.clone())
            .await
            .expect("connect console db");
        // The runner's connection is read-only, as in api-rs.
        let readonly = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options.options([("default_transaction_read_only", "on")]))
            .await
            .expect("connect read-only console db");

        let fake = Arc::new(FakeConsole {
            pool: console_db.clone(),
            clock: Mutex::new(Clock::Real),
            faults: Faults::default(),
            claims: Mutex::new(Vec::new()),
            results: Mutex::new(Vec::new()),
            result_times: Mutex::new(Vec::new()),
            undeliverable: Mutex::new(BTreeSet::new()),
            claim_missing_api_input: Mutex::new(BTreeSet::new()),
        });
        let base_url = serve_fake_console(fake.clone()).await;

        let wf_queue = format!("wf_{unique}");
        let wf_client = Client::from_pool_with_options(
            pool.clone(),
            ClientOptions {
                queue_name: wf_queue.clone(),
                ..ClientOptions::default()
            },
        )
        .unwrap();
        wf_client
            .create_queue(None, Default::default())
            .await
            .unwrap();
        let spawner = Arc::new(TestSpawner {
            client: wf_client,
            queue: wf_queue,
            requests: Mutex::new(Vec::new()),
            faults: SpawnFaults::default(),
        });

        let queue = format!("st_{unique}");
        let console = ConsoleClient::new(&base_url, SECRET, Duration::from_secs(5)).unwrap();
        let config = test_config(&queue);
        let runner = ScheduledTaskRunner::new(
            edit(config),
            pool.clone(),
            readonly,
            console,
            spawner.clone(),
        )
        .await
        .expect("runner");
        Some(Self {
            url: url.clone(),
            schema: schema.clone(),
            base_url: base_url.clone(),
            pool,
            console_db,
            fake,
            spawner,
            runner,
            queue,
        })
    }

    pub async fn insert_task(
        &self,
        cron: Option<&str>,
        timezone: &str,
        next_run_at: Option<DateTime<Utc>>,
    ) -> i64 {
        sqlx::query_scalar(
            "insert into scheduled_tasks (name, prompt, delivery_channel, cron_expression, timezone, next_run_at)
             values ($1, 'Summarize open incidents.', 'C0123456789', $2, $3, $4) returning id",
        )
        .bind(format!("Incident summary {}", uuid::Uuid::new_v4().simple()))
        .bind(cron)
        .bind(timezone)
        .bind(next_run_at.map(|at| at.naive_utc()))
        .fetch_one(&self.console_db)
        .await
        .unwrap()
    }

    pub async fn execute(&self, sql: &str, id: i64) {
        sqlx::query(sql)
            .bind(id)
            .execute(&self.console_db)
            .await
            .unwrap();
    }

    pub async fn set_next_run_at(&self, id: i64, at: Option<DateTime<Utc>>) {
        sqlx::query("update scheduled_tasks set next_run_at = $2 where id = $1")
            .bind(id)
            .bind(at.map(|at| at.naive_utc()))
            .execute(&self.console_db)
            .await
            .unwrap();
    }

    pub async fn task(&self, id: i64) -> TaskRow {
        let row = sqlx::query(
            "select enabled, next_run_at, last_enqueued_at, last_run_id, last_run_at, last_error, updated_at
             from scheduled_tasks where id = $1",
        )
        .bind(id)
        .fetch_one(&self.console_db)
        .await
        .unwrap();
        TaskRow {
            enabled: row.get("enabled"),
            next_run_at: utc(row.get("next_run_at")),
            last_enqueued_at: utc(row.get("last_enqueued_at")),
            last_run_id: row.get("last_run_id"),
            last_run_at: utc(row.get("last_run_at")),
            last_error: row.get("last_error"),
            updated_at: row.get::<NaiveDateTime, _>("updated_at").and_utc(),
        }
    }

    /// Runs every tick that is ready, including retries and chained ticks
    /// that are already due, until none is left (bounded).
    pub async fn drain(&self) {
        for _ in 0..40 {
            let ready: i64 = sqlx::query_scalar(&format!(
                "select count(*) from absurd.\"r_{}\" where state in ('pending', 'sleeping') and available_at <= now()",
                self.queue
            ))
            .fetch_one(&self.pool)
            .await
            .unwrap();
            if ready == 0 {
                return;
            }
            self.work_one().await;
        }
        panic!("ticks kept running");
    }

    /// Runs at most one ready tick.
    pub async fn work_one(&self) {
        self.runner
            .client()
            .work_batch(WorkBatchOptions {
                worker_id: "test".to_owned(),
                claim_timeout: Duration::from_secs(30),
                batch_size: 1,
            })
            .await
            .expect("work batch");
    }

    pub async fn ticks(&self) -> Vec<TickRow> {
        let rows = sqlx::query(&format!(
            "select idempotency_key, state, params, completed_payload
             from absurd.\"t_{}\" order by enqueue_at, idempotency_key",
            self.queue
        ))
        .fetch_all(&self.pool)
        .await
        .unwrap();
        rows.into_iter()
            .map(|row| TickRow {
                key: row.get("idempotency_key"),
                state: row.get("state"),
                params: row.get("params"),
                result: row.get("completed_payload"),
            })
            .collect()
    }

    pub async fn tick(&self, key: &str) -> Option<TickRow> {
        self.ticks().await.into_iter().find(|tick| tick.key == key)
    }

    pub async fn runs(&self) -> Vec<RunRow> {
        let rows = sqlx::query(&format!(
            "select idempotency_key, task_id::text as task_id, params, max_attempts
             from absurd.\"t_{}\" order by idempotency_key",
            self.spawner.queue
        ))
        .fetch_all(&self.pool)
        .await
        .unwrap();
        rows.into_iter()
            .map(|row| RunRow {
                key: row.get("idempotency_key"),
                task_id: row.get("task_id"),
                params: row.get("params"),
                max_attempts: row.get("max_attempts"),
            })
            .collect()
    }

    /// The run id `create_run` would report for a run task.
    pub async fn run_id(&self, task_id: &str) -> String {
        sqlx::query_scalar(&format!(
            "select run_id::text from absurd.\"r_{}\" where task_id = $1::uuid order by attempt limit 1",
            self.spawner.queue
        ))
        .bind(task_id)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }
}

async fn serve_fake_console(fake: Arc<FakeConsole>) -> String {
    let app = Router::new()
        .route("/internal/v1/scheduled_tasks/{oid}/claim", post(claim))
        .route("/internal/v1/scheduled_tasks/{oid}/result", post(result))
        .with_state(fake);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// The console's verification of the api-rs service token, at its clock.
fn authorized(headers: &HeaderMap, now: DateTime<Utc>) -> bool {
    let Some(token) = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    centaur_console_auth::internal::verify(token, SECRET, now)
}

/// `next_occurrence(after: now)` with `centaur_console_db::schedule`, as the console does.
fn next_occurrence(
    cron: Option<&str>,
    timezone: &str,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    cron.and_then(|cron| centaur_console_db::schedule::next_run(cron, timezone, now))
}

async fn claim(
    State(fake): State<Arc<FakeConsole>>,
    Path(oid): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ClaimRequest>,
) -> Response {
    let clock = fake.clock.lock().unwrap().clone();
    if !authorized(&headers, clock.now(None)) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    fake.claims
        .lock()
        .unwrap()
        .push((oid.clone(), request.clone()));
    if take(&fake.faults.claim_unavailable) {
        return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
    }
    let Some(id) = centaur_console_db::decode_oid("tsk", &oid) else {
        return error(StatusCode::NOT_FOUND, "scheduled task not found");
    };
    let Some(scheduled_for) = keys::parse_wire_time(&request.scheduled_for) else {
        return error(StatusCode::UNPROCESSABLE_ENTITY, "scheduled_for is invalid");
    };
    let now = clock.now(Some(scheduled_for));
    let mut tx = fake.pool.begin().await.unwrap();
    let Some(row) = sqlx::query(
        "select name, prompt, delivery_channel, cron_expression, timezone, enabled, next_run_at, last_enqueued_at
         from scheduled_tasks where id = $1 for update",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .unwrap() else {
        return error(StatusCode::NOT_FOUND, "scheduled task not found");
    };
    let enabled: bool = row.get("enabled");
    let mut next_run_at = utc(row.get("next_run_at"));
    let respond = |outcome,
                   reason: Option<&str>,
                   err: Option<String>,
                   api_input,
                   next: Option<DateTime<Utc>>| {
        Json(ClaimResponse {
            outcome,
            reason: reason.map(str::to_owned),
            error: err,
            api_input,
            enabled,
            next_run_at: next.map(keys::wire_time),
        })
        .into_response()
    };
    if !enabled {
        return respond(
            ClaimOutcome::Skipped,
            Some("task_disabled"),
            None,
            None,
            next_run_at,
        );
    }
    if !request.manual {
        if next_run_at == Some(scheduled_for) {
            let cron: Option<String> = row.get("cron_expression");
            let timezone: String = row.get("timezone");
            next_run_at = next_occurrence(cron.as_deref(), &timezone, now.max(scheduled_for));
            sqlx::query(
                "update scheduled_tasks set last_enqueued_at = $2, next_run_at = $3, updated_at = $4 where id = $1",
            )
            .bind(id)
            .bind(scheduled_for.naive_utc())
            .bind(next_run_at.map(|at| at.naive_utc()))
            .bind(now.naive_utc())
            .execute(&mut *tx)
            .await
            .unwrap();
        } else if utc(row.get("last_enqueued_at")) != Some(scheduled_for) {
            return respond(
                ClaimOutcome::Skipped,
                Some("not_due"),
                None,
                None,
                next_run_at,
            );
        }
    }
    if fake.undeliverable.lock().unwrap().contains(&id) {
        let message = "Slack delivery destination is no longer available to the task owner";
        sqlx::query("update scheduled_tasks set last_error = $2, updated_at = $3 where id = $1")
            .bind(id)
            .bind(message)
            .bind(now.naive_utc())
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        return respond(
            ClaimOutcome::Failed,
            Some("delivery_destination_unavailable"),
            Some(message.to_owned()),
            None,
            next_run_at,
        );
    }
    tx.commit().await.unwrap();
    if fake
        .claim_missing_api_input
        .lock()
        .unwrap()
        .contains(&request.scheduled_for)
    {
        return respond(ClaimOutcome::Claimed, None, None, None, next_run_at);
    }
    if take(&fake.faults.claim_commit_then_fail) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error");
    }
    let name: String = row.get("name");
    let api_input = json!({
        "prompt": row.get::<String, _>("prompt"),
        "principal": "console-user-ada-example-com-2f1c0e4a9b7d",
        "channel": row.get::<String, _>("delivery_channel"),
        "slack_user_id": "U0123456789",
        "scheduled_task_id": oid,
        "scheduled_task_name": name,
    });
    respond(
        ClaimOutcome::Claimed,
        None,
        None,
        Some(api_input),
        next_run_at,
    )
}

async fn result(
    State(fake): State<Arc<FakeConsole>>,
    Path(oid): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ResultRequest>,
) -> Response {
    let now = fake.clock.lock().unwrap().now(None);
    if !authorized(&headers, now) {
        return error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    fake.results
        .lock()
        .unwrap()
        .push((oid.clone(), request.clone()));
    fake.result_times.lock().unwrap().push(now);
    if take(&fake.faults.result_unavailable) {
        return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
    }
    let Some(id) = centaur_console_db::decode_oid("tsk", &oid) else {
        return error(StatusCode::NOT_FOUND, "scheduled task not found");
    };
    let updated = match (&request.run_id, &request.error) {
        (Some(run_id), None) => sqlx::query(
            "update scheduled_tasks set last_run_id = $2, last_run_at = $3, last_error = null, updated_at = $3
             where id = $1 returning enabled, next_run_at",
        )
        .bind(id)
        .bind(run_id)
        .bind(now.naive_utc()),
        (None, Some(message)) => sqlx::query(
            "update scheduled_tasks set last_error = $2, updated_at = $3 where id = $1 returning enabled, next_run_at",
        )
        .bind(id)
        .bind(message)
        .bind(now.naive_utc()),
        _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "exactly one of run_id and error"),
    }
    .fetch_optional(&fake.pool)
    .await
    .unwrap();
    let Some(row) = updated else {
        return error(StatusCode::NOT_FOUND, "scheduled task not found");
    };
    Json(ResultResponse {
        enabled: row.get("enabled"),
        next_run_at: utc(row.get("next_run_at")).map(keys::wire_time),
    })
    .into_response()
}
