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

pub async fn create(
    pool: &PgPool,
    queue: &str,
    task_id: &str,
    workflow_name: &str,
    step: &str,
    config: ActionConfig,
) -> Result<Value, WorkflowRuntimeError> {
    queue_table(queue)?;
    config.validate()?;
    if step.trim().is_empty() || step.len() > 200 {
        return Err(WorkflowRuntimeError::BadRequest(
            "action step name must contain 1-200 bytes".into(),
        ));
    }
    let config_json = serde_json::to_value(&config)?;
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
    Ok(json!({"id": row.try_get::<Uuid, _>("id")?}))
}

async fn resolve(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    queue: &str,
    state: &str,
    result: Value,
) -> Result<Value, WorkflowRuntimeError> {
    sqlx::query("UPDATE workflow_actions SET state = $2, result = $3, resolved_at = now(), delivery_after = CASE WHEN delivery_lease IS NULL THEN now() ELSE delivery_after END WHERE id = $1")
        .bind(id).bind(state).bind(&result).execute(&mut **tx).await?;
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
    let row = sqlx::query("SELECT * FROM workflow_actions WHERE id = $1 FOR UPDATE")
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
    // A verified click proves the message exists even if the original post's
    // response was lost. Recover its address so terminal feedback can retry.
    sqlx::query("UPDATE workflow_actions SET message_ts = coalesce(message_ts, $2), delivered_state = coalesce(delivered_state, 'pending') WHERE id = $1")
        .bind(id).bind(&invocation.message_ts).execute(&mut *tx).await?;
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

pub async fn wait(
    pool: &PgPool,
    ctx: &TaskContext,
    id: Uuid,
) -> Result<Value, WorkflowRuntimeError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query("SELECT * FROM workflow_actions WHERE id = $1 AND task_id = $2::uuid AND queue_name = $3 FOR UPDATE")
        .bind(id).bind(ctx.task_id()).bind(ctx.queue_name()).fetch_optional(&mut *tx).await?
        .ok_or_else(|| WorkflowRuntimeError::BadRequest("workflow action does not belong to this task".into()))?;
    let result = close_if_inactive(&mut tx, &row).await?;
    let seconds: f64 = sqlx::query_scalar("SELECT EXTRACT(EPOCH FROM expires_at - clock_timestamp())::float8 FROM workflow_actions WHERE id = $1")
        .bind(id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    if let Some(result) = result {
        return Ok(result);
    }
    let seconds = seconds.ceil().max(1.0) as u64;
    match ctx
        .await_event::<Value>(
            &event_name(id),
            AwaitEventOptions {
                step_name: Some(format!("$action:{id}")),
                timeout: Some(Duration::from_secs(seconds)),
            },
        )
        .await
    {
        Err(absurd::Error::Suspend) => Err(WorkflowRuntimeError::Suspend),
        Err(absurd::Error::Timeout(_)) | Ok(_) => {
            // The stored result is authoritative, never an event's payload.
            let mut tx = pool.begin().await?;
            let row = sqlx::query("SELECT * FROM workflow_actions WHERE id = $1 FOR UPDATE")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
            let result = close_if_inactive(&mut tx, &row).await?;
            tx.commit().await?;
            result.ok_or_else(|| {
                WorkflowRuntimeError::Internal("action woke without a durable result".into())
            })
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) fn start_worker(pool: PgPool) -> WorkflowTaskHeartbeatGuard {
    WorkflowTaskHeartbeatGuard {
        task: tokio::spawn(async move {
            loop {
                if let Err(error) = maintain(&pool).await {
                    warn!(%error, "workflow_action_maintenance_failed");
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }),
    }
}

pub async fn maintain(pool: &PgPool) -> Result<(), WorkflowRuntimeError> {
    // Bounded batches, including terminal tasks, ensure abandoned prompts close.
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM workflow_actions WHERE state = 'pending' ORDER BY checked_at LIMIT 100",
    )
    .fetch_all(pool)
    .await?;
    for id in ids {
        let mut tx = pool.begin().await?;
        let row =
            sqlx::query("SELECT * FROM workflow_actions WHERE id = $1 FOR UPDATE SKIP LOCKED")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(row) = row {
            close_if_inactive(&mut tx, &row).await?;
            sqlx::query("UPDATE workflow_actions SET checked_at = now() WHERE id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
    }
    for _ in 0..10 {
        if !deliver_next(pool).await? {
            break;
        }
    }
    Ok(())
}

pub(super) async fn cancel_task(
    pool: &PgPool,
    queue: &str,
    task_id: &str,
) -> Result<(), WorkflowRuntimeError> {
    queue_table(queue)?;
    let mut tx = pool.begin().await?;
    let rows = sqlx::query("SELECT id FROM workflow_actions WHERE queue_name = $1 AND task_id = $2::uuid AND state = 'pending' ORDER BY id FOR UPDATE")
        .bind(queue).bind(task_id).fetch_all(&mut *tx).await?;
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        resolve(
            &mut tx,
            id,
            queue,
            "cancelled",
            json!({"id": id, "outcome": "cancelled"}),
        )
        .await?;
    }
    sqlx::query("SELECT absurd.cancel_task($1, $2::uuid)")
        .bind(queue)
        .bind(task_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
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

async fn deliver_next(pool: &PgPool) -> Result<bool, WorkflowRuntimeError> {
    deliver_next_with(pool, send_slack_action_message).await
}

async fn deliver_next_with<F, Fut>(pool: &PgPool, send: F) -> Result<bool, WorkflowRuntimeError>
where
    F: FnOnce(&'static str, Value) -> Fut,
    Fut: Future<Output = Result<Value, WorkflowRuntimeError>>,
{
    let lease = Uuid::new_v4();
    let row = sqlx::query(
        "UPDATE workflow_actions SET delivery_lease = $1, delivery_after = now() + interval '30 seconds'
         WHERE id = (SELECT id FROM workflow_actions WHERE delivery_after <= now()
             AND delivered_state IS DISTINCT FROM state AND (message_ts IS NOT NULL OR state = 'pending')
             ORDER BY delivery_after FOR UPDATE SKIP LOCKED LIMIT 1)
         RETURNING *",
    ).bind(lease).fetch_optional(pool).await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let id: Uuid = row.try_get("id")?;
    let config: ActionConfig = serde_json::from_value(row.try_get("config")?)?;
    let result: Option<Value> = row.try_get("result")?;
    let message_ts: Option<String> = row.try_get("message_ts")?;
    let state: String = row.try_get("state")?;
    let method = if message_ts.is_some() {
        "chat.update"
    } else {
        "chat.postMessage"
    };
    let response = send(
        method,
        slack_payload(id, &config, result.as_ref(), message_ts.as_deref()),
    )
    .await;
    match response {
        Ok(response) => {
            let ts = response
                .get("ts")
                .and_then(Value::as_str)
                .or(message_ts.as_deref())
                .ok_or_else(|| {
                    WorkflowRuntimeError::Upstream(
                        "Slack action delivery missing message timestamp".into(),
                    )
                })?;
            sqlx::query("UPDATE workflow_actions SET message_ts = $3, delivered_state = $4, delivery_lease = NULL, delivery_after = now() WHERE id = $1 AND delivery_lease = $2")
                .bind(id).bind(lease).bind(ts).bind(state).execute(pool).await?;
        }
        Err(error) => {
            // Keep the retry timestamp and leave the durable outcome untouched.
            warn!(action_id = %id, %error, "workflow_action_delivery_failed");
        }
    }
    Ok(true)
}

async fn send_slack_action_message(
    method: &'static str,
    payload: Value,
) -> Result<Value, WorkflowRuntimeError> {
    let token = env::var("SLACK_BOT_TOKEN")
        .or_else(|_| env::var("SLACK_BOT_TOKEN_OVERRIDE"))
        .map_err(|_| {
            WorkflowRuntimeError::BadRequest("Slack action delivery requires a bot token".into())
        })?;
    let base = env::var("SLACK_API_URL").unwrap_or_else(|_| "https://slack.com/api/".into());
    let response: Value = reqwest::Client::new()
        .post(format!("{}/{method}", base.trim_end_matches('/')))
        .timeout(Duration::from_secs(15))
        .bearer_auth(token)
        .json(&payload)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if response.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(WorkflowRuntimeError::Upstream(format!(
            "Slack action delivery failed: {}",
            response["error"].as_str().unwrap_or("unknown_error")
        )));
    }
    Ok(response)
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
        let job = format!("action-test-{}", Uuid::new_v4());
        client.register_task(&job, {
            let pool = pool.clone();
            let callbacks = callbacks.clone();
            move |_: Value, ctx| {
                let pool = pool.clone();
                let callbacks = callbacks.clone();
                async move {
                    let prompt = create(
                        &pool,
                        ctx.queue_name(),
                        ctx.task_id(),
                        "action_test",
                        "choice",
                        config(),
                    )
                    .await
                    .map_err(absurd_error)?;
                    let id =
                        Uuid::parse_str(prompt["id"].as_str().expect("prompt id")).expect("uuid");
                    let result = wait(&pool, &ctx, id).await.map_err(absurd_error)?;
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
        // Replay preserves identity and refuses to silently change its meaning.
        let replay = create(
            &pool,
            WORKFLOW_QUEUE,
            &owner.task_id,
            "action_test",
            "choice",
            config(),
        )
        .await?;
        assert_eq!(replay["id"], id.to_string());
        let mut changed = config();
        changed.allowed_users.push("UNEW".into());
        assert!(
            create(
                &pool,
                WORKFLOW_QUEUE,
                &owner.task_id,
                "action_test",
                "choice",
                changed
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

        // A click before the owner reaches its wait is retained.
        let early = client.spawn(&job, json!({}), Default::default()).await?;
        let prompt = create(
            &pool,
            WORKFLOW_QUEUE,
            &early.task_id,
            "action_test",
            "choice",
            config(),
        )
        .await?;
        let early_id = Uuid::parse_str(prompt["id"].as_str().expect("id")).expect("uuid");
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
        let expired = client.spawn(&job, json!({}), Default::default()).await?;
        let prompt = create(
            &pool,
            WORKFLOW_QUEUE,
            &expired.task_id,
            "action_test",
            "choice",
            config(),
        )
        .await?;
        let expired_id = Uuid::parse_str(prompt["id"].as_str().expect("id")).expect("uuid");
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
        cancel_task(&pool, WORKFLOW_QUEUE, &expired.task_id).await?;

        let cancelled = client.spawn(&job, json!({}), Default::default()).await?;
        let prompt = create(
            &pool,
            WORKFLOW_QUEUE,
            &cancelled.task_id,
            "action_test",
            "choice",
            config(),
        )
        .await?;
        let cancelled_id = Uuid::parse_str(prompt["id"].as_str().expect("id")).expect("uuid");
        cancel_task(&pool, WORKFLOW_QUEUE, &cancelled.task_id).await?;
        assert_eq!(
            invoke(&pool, click(cancelled_id, "approve")).await?["result"]["outcome"],
            "cancelled"
        );

        // Prior cases invoked the API directly without a Slack transport.
        // Acknowledge their feedback before isolating delivery recovery below.
        sqlx::query("UPDATE workflow_actions SET delivered_state = state WHERE id = ANY($1)")
            .bind(vec![id, early_id, expired_id, cancelled_id])
            .execute(&pool)
            .await?;

        // A post can race with a click. The original send must retain its lease
        // and the next delivery must render the recorded terminal state.
        let delivery = client.spawn(&job, json!({}), Default::default()).await?;
        let prompt = create(
            &pool,
            WORKFLOW_QUEUE,
            &delivery.task_id,
            "action_test",
            "choice",
            config(),
        )
        .await?;
        let delivery_id = Uuid::parse_str(prompt["id"].as_str().expect("id")).expect("uuid");
        assert!(
            deliver_next_with(&pool, |method, body| {
                let pool = &pool;
                async move {
                    assert_eq!(method, "chat.postMessage");
                    assert_eq!(body["client_msg_id"], delivery_id.to_string());
                    assert_eq!(
                        body["blocks"][1]["elements"][0]["action_id"],
                        format!("{ACTION_PREFIX}{delivery_id}:approve")
                    );
                    invoke(pool, click(delivery_id, "approve")).await?;
                    let stolen = deliver_next_with(pool, |_, _| async {
                        panic!("active delivery lease was stolen")
                    })
                    .await?;
                    assert!(!stolen);
                    Ok(json!({"ok": true, "ts": "1700000000.000100"}))
                }
            })
            .await?
        );
        assert!(
            deliver_next_with(&pool, |method, body| async move {
                assert_eq!(method, "chat.update");
                assert_eq!(body["ts"], "1700000000.000100");
                assert_eq!(body["blocks"][1]["type"], "context");
                Err(WorkflowRuntimeError::Upstream("temporary failure".into()))
            })
            .await?
        );
        let state: String = sqlx::query_scalar("SELECT state FROM workflow_actions WHERE id = $1")
            .bind(delivery_id)
            .fetch_one(&pool)
            .await?;
        assert_eq!(state, "resolved");
        sqlx::query("UPDATE workflow_actions SET delivery_after = now() WHERE id = $1")
            .bind(delivery_id)
            .execute(&pool)
            .await?;
        assert!(
            deliver_next_with(&pool, |method, _| async move {
                assert_eq!(method, "chat.update");
                Ok(json!({"ok": true}))
            })
            .await?
        );
        assert!(!deliver_next_with(&pool, |_, _| async { panic!("already delivered") }).await?);
        cancel_task(&pool, WORKFLOW_QUEUE, &delivery.task_id).await?;
        Ok(())
    }
}
