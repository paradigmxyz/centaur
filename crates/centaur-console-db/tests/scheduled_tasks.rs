//! Scheduled-task runs against a Postgres database holding Console's schema.
//!
//! Set `CONSOLE_DB_TEST_DATABASE_URL` to a database migrated with
//! `cargo run -p centaur-console-db --example migrate -- <url>`. A scheduler pass claims every due task in the
//! database, so these tests run one at a time and start from an empty
//! `scheduled_tasks` table.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use centaur_console_db::{
    oid,
    scheduled_tasks::{self, ManualRun, Outcome, RunRequest, StartError, console_user_foreign_id},
};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgPool, Row, postgres::PgPoolOptions};

mod common;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}

struct Fixture {
    /// Read-only by default, as api-rs connects to Console's database.
    pool: PgPool,
    /// Read-write, for arranging and inspecting rows.
    admin_pool: PgPool,
    unique: String,
    author: i64,
    author_email: String,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let admin_pool = common::database().await?;
        let serial = SERIAL.lock().await;
        let options = (*admin_pool.connect_options())
            .clone()
            .options([("default_transaction_read_only", "on")]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("DELETE FROM scheduled_tasks")
            .execute(&admin_pool)
            .await
            .unwrap();
        let unique = format!("{:016x}", rand::random::<u64>());
        let author_email = format!("author-{unique}@example.com");
        let author = sqlx::query_scalar(
            "INSERT INTO users (email, admin, status, created_at, updated_at) \
             VALUES ($1, false, 'active', now(), now()) RETURNING id",
        )
        .bind(&author_email)
        .fetch_one(&admin_pool)
        .await
        .unwrap();
        let fixture = Self {
            pool,
            admin_pool,
            unique,
            author,
            author_email,
            _serial: serial,
        };
        Some(fixture)
    }

    /// An id unique to this test run, in a Slack ID's shape.
    fn slack_id(&self, prefix: char) -> String {
        format!("{prefix}{}", self.unique.to_uppercase())
    }

    async fn author_principal(&self) {
        sqlx::query(
            "INSERT INTO principals (foreign_id, name, kind, console_user_id, labels, created_by_id, created_at, updated_at) \
             VALUES ($1, 'Author', 'console_user', $2, '{}', $2, now(), now())",
        )
        .bind(console_user_foreign_id(&self.author_email))
        .bind(self.author)
        .execute(&self.admin_pool)
        .await
        .unwrap();
    }

    async fn slack_identity(&self) {
        sqlx::query(
            "INSERT INTO user_identities (provider, subject, team_id, user_id, created_at, updated_at) \
             VALUES ('slack', $1, $2, $3, now(), now())",
        )
        .bind(self.slack_id('U'))
        .bind(self.slack_id('T'))
        .bind(self.author)
        .execute(&self.admin_pool)
        .await
        .unwrap();
    }

    async fn channel_principal(&self, channel: &str) -> (i64, String) {
        let foreign_id = format!("slack-channel-{}", self.unique);
        let id = sqlx::query_scalar(
            "INSERT INTO principals (foreign_id, name, kind, slack_channel_id, slack_team_id, labels, created_by_id, created_at, updated_at) \
             VALUES ($1, 'Channel', 'slack_channel', $2, $3, '{}', $4, now(), now()) RETURNING id",
        )
        .bind(&foreign_id)
        .bind(channel)
        .bind(self.slack_id('T'))
        .bind(self.author)
        .fetch_one(&self.admin_pool)
        .await
        .unwrap();
        (id, foreign_id)
    }

    async fn bot_channel(&self, channel: &str, private: bool, members: &[String]) {
        sqlx::query(
            "INSERT INTO slack_bot_channels (team_id, bot_user_id, channel_id, name, private, active, member_user_ids, created_at, updated_at) \
             VALUES ($1, 'UBOT0000001', $2, $2, $3, true, $4, now(), now())",
        )
        .bind(self.slack_id('T'))
        .bind(channel)
        .bind(private)
        .bind(members)
        .execute(&self.admin_pool)
        .await
        .unwrap();
    }

    async fn task(
        &self,
        owner: Owner,
        channel: &str,
        cron: Option<&str>,
        next_run_at: Option<&str>,
    ) -> i64 {
        let (author, principal) = match owner {
            Owner::Author => (Some(self.author), None),
            Owner::Channel(principal) => (None, Some(principal)),
        };
        sqlx::query_scalar(
            "INSERT INTO scheduled_tasks (name, prompt, author_id, principal_id, delivery_channel, cron_expression, timezone, enabled, next_run_at, created_at, updated_at) \
             VALUES ($1, 'Summarize open incidents.', $2, $3, $4, $5, 'UTC', true, ($6::text::timestamptz AT TIME ZONE 'UTC'), now(), now()) RETURNING id",
        )
        .bind(format!("Task {}", rand::random::<u32>()))
        .bind(author)
        .bind(principal)
        .bind(channel)
        .bind(cron)
        .bind(next_run_at)
        .fetch_one(&self.admin_pool)
        .await
        .unwrap()
    }

    async fn state(&self, task: i64) -> TaskState {
        let row = sqlx::query(
            "SELECT next_run_at AT TIME ZONE 'UTC' AS next_run_at, \
                    last_enqueued_at AT TIME ZONE 'UTC' AS last_enqueued_at, \
                    last_run_at AT TIME ZONE 'UTC' AS last_run_at, \
                    last_run_id, last_error \
             FROM scheduled_tasks WHERE id = $1",
        )
        .bind(task)
        .fetch_one(&self.admin_pool)
        .await
        .unwrap();
        let time = |column: &str| {
            row.try_get::<Option<time::OffsetDateTime>, _>(column)
                .unwrap()
                .map(|time| {
                    time.format(&time::format_description::well_known::Rfc3339)
                        .unwrap()
                })
        };
        TaskState {
            next_run_at: time("next_run_at"),
            last_enqueued_at: time("last_enqueued_at"),
            last_run_at: time("last_run_at"),
            last_run_id: row.try_get("last_run_id").unwrap(),
            last_error: row.try_get("last_error").unwrap(),
        }
    }
}

#[derive(Clone, Copy)]
enum Owner {
    Author,
    Channel(i64),
}

#[derive(Debug, PartialEq)]
struct TaskState {
    next_run_at: Option<String>,
    last_enqueued_at: Option<String>,
    last_run_at: Option<String>,
    last_run_id: Option<String>,
    last_error: Option<String>,
}

/// Records requests and answers each with `result`.
#[derive(Clone)]
struct Starter {
    requests: Arc<Mutex<Vec<RunRequest>>>,
    result: Result<String, StartError>,
}

impl Starter {
    fn ok() -> Self {
        Self::with(Ok("run-123".to_owned()))
    }

    fn with(result: Result<String, StartError>) -> Self {
        Self {
            requests: Arc::default(),
            result,
        }
    }

    fn start(&self) -> impl Fn(RunRequest) -> std::future::Ready<Result<String, StartError>> + '_ {
        move |request| {
            self.requests.lock().unwrap().push(request);
            std::future::ready(self.result.clone())
        }
    }

    fn requests(&self) -> Vec<RunRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn runs_a_due_task_once_and_advances_its_schedule() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    fixture.author_principal().await;
    fixture.slack_identity().await;
    let channel = fixture.slack_id('C');
    fixture.bot_channel(&channel, false, &[]).await;
    let task = fixture
        .task(
            Owner::Author,
            &channel,
            Some("0 * * * *"),
            Some("2026-08-19T12:00:00Z"),
        )
        .await;
    let now = at("2026-08-19T12:00:30Z");
    let starter = Starter::ok();

    let outcomes = scheduled_tasks::run_due(&fixture.pool, now, starter.start())
        .await
        .unwrap();

    assert_eq!(
        outcomes,
        vec![Outcome::Started {
            task_id: task,
            run_id: "run-123".to_owned()
        }]
    );
    let request = starter.requests().pop().unwrap();
    assert_eq!(request.workflow_name, "console_workflow");
    assert_eq!(request.max_attempts, 3);
    assert_eq!(
        request.idempotency_key,
        format!("scheduled-task:{task}:2026-08-19T12:00:00Z")
    );
    let name = request.input["scheduled_task_name"].clone();
    assert_eq!(
        request.input,
        json!({
            "prompt": "Summarize open incidents.",
            "principal": console_user_foreign_id(&fixture.author_email),
            "channel": channel,
            "slack_user_id": fixture.slack_id('U'),
            "scheduled_task_id": oid("tsk", task),
            "scheduled_task_name": name,
        })
    );
    assert_eq!(
        fixture.state(task).await,
        TaskState {
            next_run_at: Some("2026-08-19T13:00:00Z".to_owned()),
            last_enqueued_at: Some("2026-08-19T12:00:00Z".to_owned()),
            last_run_at: Some("2026-08-19T12:00:30Z".to_owned()),
            last_run_id: Some("run-123".to_owned()),
            last_error: None,
        }
    );

    let again = scheduled_tasks::run_due(&fixture.pool, now, starter.start())
        .await
        .unwrap();
    assert!(again.is_empty());
    assert_eq!(starter.requests().len(), 1);
}

#[tokio::test]
async fn concurrent_passes_start_each_occurrence_once() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    fixture.author_principal().await;
    let channel = fixture.slack_id('C');
    fixture.bot_channel(&channel, false, &[]).await;
    fixture
        .task(
            Owner::Author,
            &channel,
            Some("0 * * * *"),
            Some("2026-08-19T12:00:00Z"),
        )
        .await;
    let now = at("2026-08-19T12:00:30Z");
    let starts = AtomicUsize::new(0);
    let start = |_request: RunRequest| {
        starts.fetch_add(1, Ordering::SeqCst);
        async {
            // Hold the claim while the other pass looks for due tasks.
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok("run-123".to_owned())
        }
    };

    let (first, second) = tokio::join!(
        scheduled_tasks::run_due(&fixture.pool, now, start),
        scheduled_tasks::run_due(&fixture.pool, now, start),
    );

    assert_eq!(first.unwrap().len() + second.unwrap().len(), 1);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_task_that_cannot_deliver_advances_without_blocking_others() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    fixture.author_principal().await;
    let channel = fixture.slack_id('C');
    let (principal, foreign_id) = fixture.channel_principal(&channel).await;
    // The bot is in no channel, so the channel task cannot deliver.
    let channel_task = fixture
        .task(
            Owner::Channel(principal),
            &channel,
            Some("0 * * * *"),
            Some("2026-08-19T11:59:00Z"),
        )
        .await;
    let dm_task = fixture
        .task(
            Owner::Author,
            &fixture.slack_id('U'),
            Some("0 * * * *"),
            Some("2026-08-19T12:00:00Z"),
        )
        .await;
    fixture.slack_identity().await;
    let starter = Starter::ok();

    let outcomes =
        scheduled_tasks::run_due(&fixture.pool, at("2026-08-19T12:00:00Z"), starter.start())
            .await
            .unwrap();

    let error = "Slack delivery destination is no longer available to the task owner";
    assert_eq!(
        outcomes,
        vec![
            Outcome::Failed {
                task_id: channel_task,
                error: error.to_owned()
            },
            Outcome::Started {
                task_id: dm_task,
                run_id: "run-123".to_owned()
            },
        ]
    );
    let state = fixture.state(channel_task).await;
    assert_eq!(state.next_run_at.as_deref(), Some("2026-08-19T13:00:00Z"));
    assert_eq!(state.last_error.as_deref(), Some(error));
    assert_eq!(starter.requests().len(), 1);

    // Once the bot is back in the channel, the task runs as the channel.
    fixture.bot_channel(&channel, false, &[]).await;
    let outcomes =
        scheduled_tasks::run_due(&fixture.pool, at("2026-08-19T13:00:00Z"), starter.start())
            .await
            .unwrap();
    assert_eq!(outcomes.len(), 2);
    let request = starter
        .requests()
        .into_iter()
        .find(|request| request.input["scheduled_task_id"] == json!(oid("tsk", channel_task)))
        .unwrap();
    assert_eq!(request.input["principal"], json!(foreign_id));
    assert_eq!(request.input["channel"], json!(channel));
    assert!(request.input.get("slack_user_id").is_none());
    assert_eq!(fixture.state(channel_task).await.last_error, None);
}

#[tokio::test]
async fn transient_start_failures_retry_until_the_window_passes() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    fixture.author_principal().await;
    let channel = fixture.slack_id('C');
    fixture.bot_channel(&channel, false, &[]).await;
    let task = fixture
        .task(
            Owner::Author,
            &channel,
            Some("0 * * * *"),
            Some("2026-08-19T12:00:00Z"),
        )
        .await;
    let starter = Starter::with(Err(StartError {
        message: "database unavailable".to_owned(),
        retryable: true,
    }));

    let outcomes =
        scheduled_tasks::run_due(&fixture.pool, at("2026-08-19T12:05:00Z"), starter.start())
            .await
            .unwrap();
    assert_eq!(
        outcomes,
        vec![Outcome::Deferred {
            task_id: task,
            error: "database unavailable".to_owned()
        }]
    );
    let state = fixture.state(task).await;
    assert_eq!(state.next_run_at.as_deref(), Some("2026-08-19T12:00:00Z"));
    assert_eq!(state.last_error.as_deref(), Some("database unavailable"));

    let outcomes =
        scheduled_tasks::run_due(&fixture.pool, at("2026-08-19T12:20:00Z"), starter.start())
            .await
            .unwrap();
    assert_eq!(
        outcomes,
        vec![Outcome::Failed {
            task_id: task,
            error: "database unavailable".to_owned()
        }]
    );
    let state = fixture.state(task).await;
    assert_eq!(state.next_run_at.as_deref(), Some("2026-08-19T13:00:00Z"));
    assert_eq!(
        state.last_enqueued_at.as_deref(),
        Some("2026-08-19T12:00:00Z")
    );
    // Every attempt named the same occurrence.
    let keys: Vec<String> = starter
        .requests()
        .into_iter()
        .map(|request| request.idempotency_key)
        .collect();
    assert_eq!(keys[0], keys[1]);
}

#[tokio::test]
async fn a_schedule_the_scheduler_cannot_read_stops_after_its_run() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    fixture.author_principal().await;
    let channel = fixture.slack_id('C');
    fixture.bot_channel(&channel, false, &[]).await;
    let task = fixture
        .task(
            Owner::Author,
            &channel,
            Some("0 9 L * *"),
            Some("2026-08-19T12:00:00Z"),
        )
        .await;
    let starter = Starter::ok();

    scheduled_tasks::run_due(&fixture.pool, at("2026-08-19T12:00:00Z"), starter.start())
        .await
        .unwrap();

    let state = fixture.state(task).await;
    assert_eq!(state.last_run_id.as_deref(), Some("run-123"));
    assert_eq!(state.next_run_at, None);
    assert!(
        state
            .last_error
            .as_deref()
            .is_some_and(|error| error.starts_with("schedule is invalid")),
        "{state:?}"
    );
}

#[tokio::test]
async fn disabled_and_manual_tasks_are_not_scheduled() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    fixture.author_principal().await;
    let channel = fixture.slack_id('C');
    fixture.bot_channel(&channel, false, &[]).await;
    let disabled = fixture
        .task(
            Owner::Author,
            &channel,
            Some("0 * * * *"),
            Some("2026-08-19T12:00:00Z"),
        )
        .await;
    sqlx::query("UPDATE scheduled_tasks SET enabled = false WHERE id = $1")
        .bind(disabled)
        .execute(&fixture.admin_pool)
        .await
        .unwrap();
    fixture.task(Owner::Author, &channel, None, None).await;
    let starter = Starter::ok();

    let outcomes =
        scheduled_tasks::run_due(&fixture.pool, at("2026-08-19T12:00:00Z"), starter.start())
            .await
            .unwrap();
    assert!(outcomes.is_empty());

    let run = scheduled_tasks::run_now(&fixture.pool, disabled, None, Utc::now(), starter.start())
        .await
        .unwrap();
    assert_eq!(run, ManualRun::Disabled);
    assert!(starter.requests().is_empty());
}

#[tokio::test]
async fn runs_a_task_on_demand_without_moving_its_schedule() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    fixture.author_principal().await;
    let channel = fixture.slack_id('C');
    fixture.bot_channel(&channel, false, &[]).await;
    let task = fixture.task(Owner::Author, &channel, None, None).await;
    let starter = Starter::ok();
    let now = at("2026-08-19T12:05:00.250Z");

    let run = scheduled_tasks::run_now(&fixture.pool, task, None, now, starter.start())
        .await
        .unwrap();

    assert_eq!(
        run,
        ManualRun::Started {
            run_id: "run-123".to_owned()
        }
    );
    assert_eq!(
        starter.requests()[0].idempotency_key,
        format!("scheduled-task:{task}:2026-08-19T12:05:00Z")
    );
    let state = fixture.state(task).await;
    assert_eq!(state.last_run_id.as_deref(), Some("run-123"));
    assert_eq!(
        state.last_run_at.as_deref(),
        Some("2026-08-19T12:05:00.25Z")
    );
    assert_eq!(state.next_run_at, None);
    assert_eq!(state.last_enqueued_at, None);

    let missing =
        scheduled_tasks::run_now(&fixture.pool, task + 1_000_000, None, now, starter.start())
            .await
            .unwrap();
    assert_eq!(missing, ManualRun::NotFound);
}

#[tokio::test]
async fn private_channels_need_both_the_author_and_the_bot() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    fixture.author_principal().await;
    fixture.slack_identity().await;
    let author = fixture.slack_id('U');
    let channel = fixture.slack_id('G');
    fixture
        .bot_channel(&channel, true, &["UBOT0000001".to_owned()])
        .await;
    let task = fixture.task(Owner::Author, &channel, None, None).await;
    let starter = Starter::ok();

    let refused = scheduled_tasks::run_now(&fixture.pool, task, None, Utc::now(), starter.start())
        .await
        .unwrap();
    assert!(matches!(
        refused,
        ManualRun::Failed(StartError {
            retryable: false,
            ..
        })
    ));
    assert!(starter.requests().is_empty());

    sqlx::query("UPDATE slack_bot_channels SET member_user_ids = $2 WHERE channel_id = $1")
        .bind(&channel)
        .bind(vec!["UBOT0000001".to_owned(), author])
        .execute(&fixture.admin_pool)
        .await
        .unwrap();
    let run = scheduled_tasks::run_now(&fixture.pool, task, None, Utc::now(), starter.start())
        .await
        .unwrap();
    assert!(matches!(run, ManualRun::Started { .. }));
    assert_eq!(fixture.state(task).await.last_error, None);
}

#[tokio::test]
async fn an_author_without_a_principal_cannot_run() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let channel = fixture.slack_id('C');
    fixture.bot_channel(&channel, false, &[]).await;
    let task = fixture.task(Owner::Author, &channel, None, None).await;
    let starter = Starter::ok();

    let run = scheduled_tasks::run_now(&fixture.pool, task, None, Utc::now(), starter.start())
        .await
        .unwrap();

    let ManualRun::Failed(error) = run else {
        panic!("expected a failure, got {run:?}");
    };
    assert!(error.message.contains("no Console principal"), "{error:?}");
    assert!(starter.requests().is_empty());
}
