//! Durable workflow-owned button callbacks. Python replays the owner workflow
//! and dispatches the selected callback; no executable code crosses this API.
use super::*;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

pub const ACTION_PREFIX: &str = "centaur.workflow.action:";
const EVENT_PREFIX: &str = "centaur.internal.workflow.action:";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ActionButton {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub style: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ActionConfig {
    pub team_id: String,
    pub channel: String,
    pub text: String,
    #[serde(default)]
    pub thread_ts: Option<String>,
    pub allowed_users: Vec<String>,
    pub buttons: Vec<ActionButton>,
    pub timeout_seconds: u32,
}

impl ActionConfig {
    fn validate(&self) -> Result<(), WorkflowRuntimeError> {
        let valid = !self.team_id.trim().is_empty()
            && !self.channel.trim().is_empty()
            && !self.text.trim().is_empty()
            && self.text.chars().count() <= 3000
            && (1..=100).contains(&self.allowed_users.len())
            && self.allowed_users.iter().all(|id| !id.trim().is_empty())
            && (1..=5).contains(&self.buttons.len())
            && (1..=2_592_000).contains(&self.timeout_seconds)
            && self.buttons.iter().all(|button| {
                !button.id.is_empty()
                    && button.id.len() <= 64
                    && button
                        .id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                    && !button.label.trim().is_empty()
                    && button.label.chars().count() <= 75
                    && matches!(button.style.as_deref(), None | Some("primary" | "danger"))
            })
            && self
                .buttons
                .iter()
                .map(|b| &b.id)
                .collect::<BTreeSet<_>>()
                .len()
                == self.buttons.len();
        if !valid {
            return Err(WorkflowRuntimeError::BadRequest(
                "invalid workflow action configuration".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct ActionInvocation {
    pub action_id: String,
    pub team_id: String,
    pub channel_id: String,
    pub user_id: String,
    pub message_ts: String,
    pub action_ts: String,
}

fn event_name(id: Uuid) -> String {
    format!("{EVENT_PREFIX}{id}")
}

pub fn is_internal_event(name: &str) -> bool {
    name.starts_with(EVENT_PREFIX)
}

fn queue_table(queue: &str) -> Result<String, WorkflowRuntimeError> {
    if ![
        WORKFLOW_QUEUE,
        WORKFLOW_SLACK_LIVE_QUEUE,
        WORKFLOW_ETL_QUEUE,
        WORKFLOW_ETL_BACKFILL_QUEUE,
    ]
    .contains(&queue)
    {
        return Err(WorkflowRuntimeError::BadRequest(
            "invalid workflow action queue".into(),
        ));
    }
    Ok(format!("absurd.t_{queue}"))
}

async fn create(
    pool: &PgPool,
    queue: &str,
    task_id: &str,
    workflow_name: &str,
    step: &str,
    config: &ActionConfig,
) -> Result<Uuid, WorkflowRuntimeError> {
    queue_table(queue)?;
    config.validate()?;
    if step.trim().is_empty() || step.len() > 200 {
        return Err(WorkflowRuntimeError::BadRequest(
            "action step name must contain 1-200 bytes".into(),
        ));
    }
    let config_json = serde_json::to_value(config)?;
    let row = sqlx::query(
        "INSERT INTO workflow_actions (id, queue_name, task_id, workflow_name, step_name, config, expires_at)
         VALUES ($1, $2, $3::uuid, $4, $5, $6, now() + make_interval(secs => $7))
         ON CONFLICT (queue_name, task_id, step_name) DO UPDATE SET step_name = excluded.step_name
         RETURNING id, config",
    )
    .bind(Uuid::new_v4()).bind(queue).bind(task_id).bind(workflow_name).bind(step)
    .bind(&config_json).bind(f64::from(config.timeout_seconds)).fetch_one(pool).await?;
    if row.try_get::<Value, _>("config")? != config_json {
        return Err(WorkflowRuntimeError::BadRequest(
            "workflow action configuration changed during replay; use a new step name".into(),
        ));
    }
    Ok(row.try_get("id")?)
}

async fn resolve(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    queue: &str,
    state: &str,
    result: Value,
) -> Result<Value, WorkflowRuntimeError> {
    sqlx::query(
        "UPDATE workflow_actions SET state = $2, result = $3, resolved_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(state)
    .bind(&result)
    .execute(&mut **tx)
    .await?;
    // State and wakeup commit together. Absurd retains early events and applies
    // the first event once, so a click before wait registration is safe.
    sqlx::query("SELECT absurd.emit_event($1, $2, $3::jsonb)")
        .bind(queue)
        .bind(event_name(id))
        .bind(json!({"id": id}))
        .execute(&mut **tx)
        .await?;
    Ok(result)
}

async fn close_if_inactive(
    tx: &mut Transaction<'_, Postgres>,
    row: &sqlx::postgres::PgRow,
) -> Result<Option<Value>, WorkflowRuntimeError> {
    if let Some(result) = row.try_get::<Option<Value>, _>("result")? {
        return Ok(Some(result));
    }
    let id: Uuid = row.try_get("id")?;
    let queue: String = row.try_get("queue_name")?;
    let table = queue_table(&queue)?;
    let task_state: Option<String> =
        sqlx::query_scalar(&format!("SELECT state FROM {table} WHERE task_id = $1"))
            .bind(row.try_get::<Uuid, _>("task_id")?)
            .fetch_optional(&mut **tx)
            .await?;
    // Check database wall time after acquiring the group lock, including when
    // an invocation spent time waiting behind another transaction.
    let expired: bool = sqlx::query_scalar(
        "SELECT expires_at <= clock_timestamp() FROM workflow_actions WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    let state = if task_state
        .as_deref()
        .is_none_or(|state| matches!(state, "completed" | "failed" | "cancelled"))
    {
        Some("cancelled")
    } else if expired {
        Some("expired")
    } else {
        None
    };
    match state {
        Some(state) => resolve(tx, id, &queue, state, json!({"id": id, "outcome": state}))
            .await
            .map(Some),
        None => Ok(None),
    }
}

pub async fn invoke(
    pool: &PgPool,
    invocation: ActionInvocation,
) -> Result<Value, WorkflowRuntimeError> {
    let Some((id, choice)) = invocation
        .action_id
        .strip_prefix(ACTION_PREFIX)
        .and_then(|s| s.split_once(':'))
    else {
        return Err(WorkflowRuntimeError::BadRequest(
            "invalid workflow action ID".into(),
        ));
    };
    let id = Uuid::parse_str(id)
        .map_err(|_| WorkflowRuntimeError::BadRequest("invalid workflow action ID".into()))?;
    if invocation.action_ts.is_empty()
        || invocation.message_ts.is_empty()
        || invocation.user_id.is_empty()
    {
        return Err(WorkflowRuntimeError::BadRequest(
            "workflow action requires click, message, and user identity".into(),
        ));
    }
    let mut tx = pool.begin().await?;
    let row = sqlx::query("SELECT id, queue_name, task_id, workflow_name, config, result FROM workflow_actions WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(row) = row else {
        return Ok(json!({"outcome": "unavailable"}));
    };
    let config: ActionConfig = serde_json::from_value(row.try_get("config")?)?;
    if config.team_id != invocation.team_id
        || config.channel != invocation.channel_id
        || !config.allowed_users.contains(&invocation.user_id)
    {
        return Ok(json!({"outcome": "forbidden"}));
    }
    // The group is valid before its original post response is stored. An
    // ambiguous retried post still references this same single-use group.
    if !config.buttons.iter().any(|b| b.id == choice) {
        return Ok(json!({"outcome": "unavailable"}));
    }
    let workflow_name: String = row.try_get("workflow_name")?;
    if WorkflowEnablement::from_env()?
        .ensure_enabled(&workflow_name)
        .is_err()
    {
        return Ok(json!({"outcome": "unavailable"}));
    }
    let existing = close_if_inactive(&mut tx, &row).await?;
    if let Some(result) = existing {
        tx.commit().await?;
        return Ok(json!({"outcome": "already_resolved", "result": result}));
    }
    let result = json!({
        "id": id, "outcome": "clicked", "action": choice,
        "user_id": invocation.user_id, "team_id": invocation.team_id,
        "channel_id": invocation.channel_id, "message_ts": invocation.message_ts,
        "action_ts": invocation.action_ts,
    });
    resolve(
        &mut tx,
        id,
        &row.try_get::<String, _>("queue_name")?,
        "resolved",
        result.clone(),
    )
    .await?;
    tx.commit().await?;
    Ok(json!({"outcome": "accepted", "result": result}))
}

async fn wait(pool: &PgPool, ctx: &TaskContext, id: Uuid) -> Result<Value, WorkflowRuntimeError> {
    let mut resumed = false;
    loop {
        let mut tx = pool.begin().await?;
        let row = sqlx::query("SELECT id, queue_name, task_id, workflow_name, config, result FROM workflow_actions WHERE id = $1 AND task_id = $2::uuid AND queue_name = $3 FOR UPDATE")
            .bind(id).bind(ctx.task_id()).bind(ctx.queue_name()).fetch_optional(&mut *tx).await?
            .ok_or_else(|| WorkflowRuntimeError::BadRequest("workflow action does not belong to this task".into()))?;
        let result = close_if_inactive(&mut tx, &row).await?;
        let seconds: f64 = sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM expires_at - clock_timestamp())::float8 FROM workflow_actions WHERE id = $1")
            .bind(id).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        // Read authoritative state both before waiting and after wakeup.
        if let Some(result) = result {
            return Ok(result);
        }
        if resumed {
            return Err(WorkflowRuntimeError::Internal(
                "action woke without a durable result".into(),
            ));
        }
        match ctx
            .await_event::<Value>(
                &event_name(id),
                AwaitEventOptions {
                    step_name: Some(format!("$action:{id}")),
                    timeout: Some(Duration::from_secs(seconds.ceil().max(1.0) as u64)),
                },
            )
            .await
        {
            Err(absurd::Error::Suspend) => return Err(WorkflowRuntimeError::Suspend),
            Err(absurd::Error::Timeout(_)) | Ok(_) => resumed = true,
            Err(error) => return Err(error.into()),
        }
    }
}

/// Register, post, wait, and update using the owning task's checkpoints.
pub async fn run(
    pool: &PgPool,
    ctx: &TaskContext,
    workflow_name: &str,
    step: &str,
    config: ActionConfig,
) -> Result<Value, WorkflowRuntimeError> {
    run_with(pool, ctx, workflow_name, step, config, send_slack_request).await
}

async fn run_with<F, Fut>(
    pool: &PgPool,
    ctx: &TaskContext,
    workflow_name: &str,
    step: &str,
    config: ActionConfig,
    send: F,
) -> Result<Value, WorkflowRuntimeError>
where
    F: Fn(&'static str, Value) -> Fut + Sync,
    Fut: Future<Output = Result<Value, WorkflowRuntimeError>> + Send,
{
    let id = create(
        pool,
        ctx.queue_name(),
        ctx.task_id(),
        workflow_name,
        step,
        &config,
    )
    .await?;
    let message_ts = ctx
        .step(&format!("$action:{id}:post"), || async {
            // A click can arrive before a successful post response is checkpointed.
            // Reuse its verified message address when recovering that window.
            let result: Option<Value> =
                sqlx::query_scalar("SELECT result FROM workflow_actions WHERE id = $1")
                    .bind(id)
                    .fetch_one(pool)
                    .await
                    .map_err(|error| absurd_error(error.into()))?;
            if let Some(ts) = result
                .as_ref()
                .and_then(|r| r.get("message_ts"))
                .and_then(Value::as_str)
            {
                return Ok(ts.to_owned());
            }
            let response = send(
                "chat.postMessage",
                slack_payload(id, &config, result.as_ref(), None),
            )
            .await
            .map_err(absurd_error)?;
            response
                .get("ts")
                .and_then(Value::as_str)
                .filter(|ts| !ts.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    absurd_error(WorkflowRuntimeError::Upstream(
                        "Slack action delivery missing message timestamp".into(),
                    ))
                })
        })
        .await?;
    let result = wait(pool, ctx, id).await?;
    ctx.step(&format!("$action:{id}:update"), || async {
        send(
            "chat.update",
            slack_payload(id, &config, Some(&result), Some(&message_ts)),
        )
        .await
        .map_err(absurd_error)?;
        Ok(())
    })
    .await?;
    Ok(result)
}

fn slack_payload(
    id: Uuid,
    config: &ActionConfig,
    result: Option<&Value>,
    message_ts: Option<&str>,
) -> Value {
    let mut payload = json!({"channel": config.channel, "text": config.text});
    let mut blocks =
        vec![json!({"type": "section", "text": {"type": "mrkdwn", "text": config.text}})];
    if let Some(result) = result {
        let text = if let Some(action) = result.get("action").and_then(Value::as_str) {
            let label = config
                .buttons
                .iter()
                .find(|b| b.id == action)
                .map_or(action, |b| b.label.as_str());
            format!(
                "{} selected by <@{}>.",
                label,
                result["user_id"].as_str().unwrap_or("")
            )
        } else {
            format!(
                "This request is {}.",
                result["outcome"].as_str().unwrap_or("closed")
            )
        };
        blocks.push(json!({"type": "context", "elements": [{"type": "mrkdwn", "text": text}]}));
    } else {
        let buttons: Vec<Value> = config.buttons.iter().map(|button| {
            let mut value = json!({"type": "button", "text": {"type": "plain_text", "text": button.label},
                "action_id": format!("{ACTION_PREFIX}{id}:{}", button.id)});
            if let Some(style) = &button.style { value["style"] = json!(style); }
            value
        }).collect();
        blocks.push(json!({"type": "actions", "elements": buttons}));
    }
    payload["blocks"] = json!(blocks);
    if let Some(ts) = message_ts {
        payload["ts"] = json!(ts);
    } else {
        payload["client_msg_id"] = json!(id);
        if let Some(ts) = &config.thread_ts {
            payload["thread_ts"] = json!(ts);
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use absurd::{TaskResultSnapshot, WorkBatchOptions};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn config() -> ActionConfig {
        ActionConfig {
            team_id: "TTEST".into(),
            channel: "CTEST".into(),
            text: "Choose an action".into(),
            thread_ts: None,
            allowed_users: vec!["UALLOWED".into()],
            timeout_seconds: 3600,
            buttons: vec![
                ActionButton {
                    id: "approve".into(),
                    label: "Approve".into(),
                    style: Some("primary".into()),
                },
                ActionButton {
                    id: "reject".into(),
                    label: "Reject".into(),
                    style: None,
                },
            ],
        }
    }

    fn click(id: Uuid, choice: &str) -> ActionInvocation {
        ActionInvocation {
            action_id: format!("{ACTION_PREFIX}{id}:{choice}"),
            team_id: "TTEST".into(),
            channel_id: "CTEST".into(),
            user_id: "UALLOWED".into(),
            message_ts: "1700000000.000100".into(),
            action_ts: "1700000001.000100".into(),
        }
    }

    async fn spawn_action(
        client: &Client,
        job: &str,
    ) -> Result<(absurd::SpawnResult, Uuid), WorkflowRuntimeError> {
        let owner = client.spawn(job, json!({}), Default::default()).await?;
        let id = create(
            client.pool(),
            WORKFLOW_QUEUE,
            &owner.task_id,
            "action_test",
            "choice",
            &config(),
        )
        .await?;
        Ok((owner, id))
    }

    #[test]
    fn action_configuration_rejects_ambiguous_or_unbounded_buttons() {
        let mut value = config();
        assert!(value.validate().is_ok());
        value.buttons[1].id = "approve".into();
        assert!(value.validate().is_err());
        value = config();
        value.allowed_users.clear();
        assert!(value.validate().is_err());
        value = config();
        value.timeout_seconds = 0;
        assert!(value.validate().is_err());
    }

    #[tokio::test]
    async fn database_action_lifecycle_and_recovery() -> Result<(), WorkflowRuntimeError> {
        let Ok(url) = env::var("ABSURD_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping workflow action database test: ABSURD_TEST_DATABASE_URL is not set"
            );
            return Ok(());
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(6)
            .connect(&url)
            .await?;
        let schema: Option<String> = sqlx::query_scalar("SELECT to_regnamespace('absurd')::text")
            .fetch_one(&pool)
            .await?;
        if schema.is_none() {
            sqlx::raw_sql(include_str!(
                "../../centaur-session-sqlx/migrations/0007_absurd_workflows.sql"
            ))
            .execute(&pool)
            .await?;
            sqlx::raw_sql(include_str!(
                "../../centaur-session-sqlx/migrations/0009_absurd_await_event_task_guard.sql"
            ))
            .execute(&pool)
            .await?;
        }
        let table: Option<String> =
            sqlx::query_scalar("SELECT to_regclass('workflow_actions')::text")
                .fetch_one(&pool)
                .await?;
        if table.is_none() {
            sqlx::raw_sql(include_str!(
                "../../centaur-session-sqlx/migrations/0055_workflow_actions.sql"
            ))
            .execute(&pool)
            .await?;
        }
        let client = Client::from_pool_with_options(
            pool.clone(),
            ClientOptions {
                queue_name: WORKFLOW_QUEUE.into(),
                ..Default::default()
            },
        )?;
        client.create_queue(None, Default::default()).await?;
        let callbacks = Arc::new(AtomicUsize::new(0));
        let posts = Arc::new(AtomicUsize::new(0));
        let updates = Arc::new(AtomicUsize::new(0));
        let job = format!("action-test-{}", Uuid::new_v4());
        client.register_task(&job, {
            let pool = pool.clone();
            let callbacks = callbacks.clone();
            let posts = posts.clone();
            let updates = updates.clone();
            move |_: Value, ctx| {
                let pool = pool.clone();
                let callbacks = callbacks.clone();
                let posts = posts.clone();
                let updates = updates.clone();
                async move {
                    let result = run_with(
                        &pool,
                        &ctx,
                        "action_test",
                        "choice",
                        config(),
                        |method, body| {
                            let posts = posts.clone();
                            let updates = updates.clone();
                            async move {
                                if method == "chat.postMessage" {
                                    posts.fetch_add(1, Ordering::SeqCst);
                                    assert_eq!(body["blocks"][1]["type"], "actions");
                                } else {
                                    updates.fetch_add(1, Ordering::SeqCst);
                                    assert_eq!(body["ts"], "1700000000.000100");
                                    assert_eq!(body["blocks"][1]["type"], "context");
                                }
                                Ok(json!({"ok": true, "ts": "1700000000.000100"}))
                            }
                        },
                    )
                    .await
                    .map_err(absurd_error)?;
                    ctx.step("callback", || async move {
                        callbacks.fetch_add(1, Ordering::SeqCst);
                        Ok(result)
                    })
                    .await
                }
            }
        })?;
        let owner = client.spawn(&job, json!({}), Default::default()).await?;
        client.work_batch(WorkBatchOptions::default()).await?;
        assert!(matches!(
            client.fetch_task_result(&owner.task_id, None).await?,
            Some(TaskResultSnapshot::Sleeping)
        ));
        let id: Uuid =
            sqlx::query_scalar("SELECT id FROM workflow_actions WHERE task_id = $1::uuid")
                .bind(&owner.task_id)
                .fetch_one(&pool)
                .await?;
        // Upgrade a suspended action without losing its identity or post checkpoint.
        sqlx::raw_sql(include_str!(
            "../../centaur-session-sqlx/migrations/0056_workflow_action_steps.sql"
        ))
        .execute(&pool)
        .await?;
        // Replay preserves identity and refuses to silently change its meaning.
        let replay = create(
            &pool,
            WORKFLOW_QUEUE,
            &owner.task_id,
            "action_test",
            "choice",
            &config(),
        )
        .await?;
        assert_eq!(replay, id);
        let mut changed = config();
        changed.allowed_users.push("UNEW".into());
        assert!(
            create(
                &pool,
                WORKFLOW_QUEUE,
                &owner.task_id,
                "action_test",
                "choice",
                &changed
            )
            .await
            .is_err()
        );
        for field in ["user", "team", "channel"] {
            let mut denied = click(id, "approve");
            match field {
                "user" => denied.user_id = "UOTHER".into(),
                "team" => denied.team_id = "TOTHER".into(),
                _ => denied.channel_id = "COTHER".into(),
            }
            assert_eq!(invoke(&pool, denied).await?["outcome"], "forbidden");
        }
        assert_eq!(
            invoke(&pool, click(id, "tampered")).await?["outcome"],
            "unavailable"
        );
        let (approve, reject) = tokio::join!(
            invoke(&pool, click(id, "approve")),
            invoke(&pool, click(id, "reject"))
        );
        let results = [approve?, reject?];
        assert_eq!(
            results
                .iter()
                .filter(|r| r["outcome"] == "accepted")
                .count(),
            1
        );
        let winner = results
            .iter()
            .find(|r| r["outcome"] == "accepted")
            .expect("winner")["result"]
            .clone();
        let retried = invoke(&pool, click(id, "approve")).await?;
        assert_eq!(retried["outcome"], "already_resolved");
        assert_eq!(retried["result"], winner);
        // Reconnect before replay: all information needed to wake is in Postgres.
        client.work_batch(WorkBatchOptions::default()).await?;
        assert_eq!(
            client
                .fetch_task_result(&owner.task_id, None)
                .await?
                .expect("task")
                .result::<Value>()?,
            Some(winner)
        );
        assert_eq!(callbacks.load(Ordering::SeqCst), 1);
        assert_eq!(posts.load(Ordering::SeqCst), 1);
        assert_eq!(updates.load(Ordering::SeqCst), 1);

        // A click before the owner reaches its wait is retained.
        let (early, early_id) = spawn_action(&client, &job).await?;
        assert_eq!(
            invoke(&pool, click(early_id, "reject")).await?["outcome"],
            "accepted"
        );
        client.work_batch(WorkBatchOptions::default()).await?;
        assert_eq!(
            client
                .fetch_task_result(&early.task_id, None)
                .await?
                .expect("task")
                .result::<Value>()?
                .expect("result")["action"],
            "reject"
        );

        // Expiry is enforced on invocation without relying on a worker tick.
        let (expired, expired_id) = spawn_action(&client, &job).await?;
        sqlx::query(
            "UPDATE workflow_actions SET expires_at = now() - interval '1 second' WHERE id = $1",
        )
        .bind(expired_id)
        .execute(&pool)
        .await?;
        assert_eq!(
            invoke(&pool, click(expired_id, "approve")).await?["result"]["outcome"],
            "expired"
        );
        client.cancel_task(&expired.task_id, None).await?;

        let (cancelled, cancelled_id) = spawn_action(&client, &job).await?;
        client.cancel_task(&cancelled.task_id, None).await?;
        assert_eq!(
            invoke(&pool, click(cancelled_id, "approve")).await?["result"]["outcome"],
            "cancelled"
        );

        // A lost post response and a failed message update use normal task
        // retries. The accepted click and successful checkpoints survive both.
        let recovery_job = format!("action-recovery-{}", Uuid::new_v4());
        let sends = Arc::new(AtomicUsize::new(0));
        let recovered_callbacks = Arc::new(AtomicUsize::new(0));
        client.register_task(&recovery_job, {
            let pool = pool.clone();
            let sends = sends.clone();
            let callbacks = recovered_callbacks.clone();
            move |_: Value, ctx| {
                let pool = pool.clone();
                let sends = sends.clone();
                let callbacks = callbacks.clone();
                async move {
                    let result = run_with(
                        &pool,
                        &ctx,
                        "action_test",
                        "recover",
                        config(),
                        |method, body| {
                            let pool = pool.clone();
                            let sends = sends.clone();
                            async move {
                                match sends.fetch_add(1, Ordering::SeqCst) {
                                    0 => {
                                        assert_eq!(method, "chat.postMessage");
                                        let id =
                                            serde_json::from_value(body["client_msg_id"].clone())?;
                                        assert_eq!(
                                            invoke(&pool, click(id, "approve")).await?["outcome"],
                                            "accepted"
                                        );
                                        Err(WorkflowRuntimeError::Upstream(
                                            "lost post response".into(),
                                        ))
                                    }
                                    1 => {
                                        assert_eq!(method, "chat.update");
                                        Err(WorkflowRuntimeError::Upstream(
                                            "temporary update failure".into(),
                                        ))
                                    }
                                    2 => {
                                        assert_eq!(method, "chat.update");
                                        assert_eq!(body["ts"], "1700000000.000100");
                                        Ok(json!({"ok": true}))
                                    }
                                    _ => panic!("checkpointed delivery repeated"),
                                }
                            }
                        },
                    )
                    .await
                    .map_err(absurd_error)?;
                    ctx.step("callback", || async {
                        callbacks.fetch_add(1, Ordering::SeqCst);
                        Ok(result)
                    })
                    .await
                }
            }
        })?;
        let recovery = client
            .spawn(
                &recovery_job,
                json!({}),
                SpawnOptions {
                    max_attempts: Some(3),
                    retry_strategy: Some(absurd::RetryStrategy {
                        kind: absurd::RetryKind::Fixed,
                        base_seconds: Some(0.0),
                        factor: None,
                        max_seconds: None,
                    }),
                    ..Default::default()
                },
            )
            .await?;
        for _ in 0..3 {
            client.work_batch(WorkBatchOptions::default()).await?;
        }
        let recovered = client
            .fetch_task_result(&recovery.task_id, None)
            .await?
            .expect("recovery task")
            .result::<Value>()?
            .expect("recovered result");
        assert_eq!(recovered["action"], "approve");
        assert_eq!(sends.load(Ordering::SeqCst), 3);
        assert_eq!(recovered_callbacks.load(Ordering::SeqCst), 1);
        Ok(())
    }
}
