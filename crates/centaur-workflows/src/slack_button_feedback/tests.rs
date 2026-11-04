use super::*;
use absurd::{Client, ClientOptions, SpawnOptions, TaskResultSnapshot, WorkBatchOptions};
use std::{sync::atomic::AtomicUsize, time::Duration};
use tokio::sync::Mutex;

fn fixture() -> ButtonFeedback {
    let id = "00000000-0000-0000-0000-000000000001";
    let mut message = json!({"channel": "C1", "text": "Approve release?", "blocks": [
        {"type": "section", "text": {"type": "mrkdwn", "text": "Approve release?"}},
        {"type": "actions", "elements": (["approve", "reject"].map(|action| json!({
            "type": "button", "text": {"type": "plain_text", "text": action},
            "action_id": format!("centaur.workflow.action:{id}:{action}"),
            "value": json!({"workflow_name": "review", "input": {}}).to_string()
        })))}
    ]});
    crate::slack_buttons::sign_message(&mut message, b"fixture-key").unwrap();
    let invocation = crate::slack_buttons::Invocation {
        button: message["blocks"][1]["elements"][0]["value"]
            .as_str()
            .unwrap()
            .into(),
        click: json!({"id": id, "action": "approve", "channel_id": "C1", "message_ts": "1.1"}),
        idempotency_key: "click-1".into(),
        message: Some(message),
    };
    ButtonFeedback::from_invocation(&invocation).unwrap()
}

#[test]
fn busy_state_replaces_the_button_row_and_preserves_the_message() {
    let feedback = fixture();
    assert_eq!(
        feedback.processing["blocks"][0],
        feedback.original["blocks"][0]
    );
    assert_eq!(
        feedback.processing["blocks"][1],
        json!({"type": "context", "elements": [
            {"type": "plain_text", "text": "⏳ approve: Processing…"}
        ]})
    );
    assert_eq!(
        feedback.processing["text"],
        "Approve release?\n⏳ approve: Processing…"
    );
    assert_eq!(
        feedback.original["blocks"][1]["elements"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
enum Scenario {
    Rejection,
    Failure,
    Result,
    Replay,
    FailureAfterResult,
    ProcessingError,
    RestoreError,
    CheckpointWriteError,
    FailureAfterCheckpointWriteError,
    CheckpointReadError,
}

impl Scenario {
    fn fails_checkpoint_write(self) -> bool {
        matches!(
            self,
            Self::CheckpointWriteError | Self::FailureAfterCheckpointWriteError
        )
    }

    fn fails_handler(self) -> bool {
        matches!(
            self,
            Self::Failure | Self::FailureAfterResult | Self::FailureAfterCheckpointWriteError
        )
    }
}

#[derive(Deserialize, Serialize)]
struct ScenarioInput {
    feedback: ButtonFeedback,
    scenario: Scenario,
}

async fn run_scenario(
    input: ScenarioInput,
    ctx: TaskContext,
    edits: Arc<Mutex<Vec<Value>>>,
    handler_calls: Arc<AtomicUsize>,
) -> absurd::Result<Value> {
    let ScenarioInput { feedback, scenario } = input;
    let fail_processing = scenario == Scenario::ProcessingError;
    let fail_restore = scenario == Scenario::RestoreError;
    let processing = feedback.processing.clone();
    let original = feedback.original.clone();
    let send = move |message: Value| {
        let edits = edits.clone();
        let fail =
            (fail_processing && message == processing) || (fail_restore && message == original);
        async move {
            if fail {
                return Err(WorkflowRuntimeError::Upstream("Slack edit failed".into()));
            }
            edits.lock().await.push(message);
            Ok(json!({"ok": true}))
        }
    };
    let ctx = &ctx;
    run(Some(feedback), ctx, send.clone(), |feedback| async move {
        // Deliberately not checkpointed: cosmetic failures must
        // not cause the engine to execute this side effect again.
        handler_calls.fetch_add(1, Ordering::SeqCst);
        if scenario == Scenario::Failure {
            return Err(absurd::Error::InvalidOptions("handler failed".into()));
        }
        if !matches!(scenario, Scenario::Rejection | Scenario::RestoreError) {
            ctx.step("workflow-result", || async {
                let message =
                    json!({"channel": "C1", "ts": "1.1", "text": "Approved", "blocks": []});
                let response =
                    update(feedback.as_ref(), ctx, &message, send(message.clone())).await;
                if scenario.fails_checkpoint_write() {
                    let error: String = response.expect_err("marker write must fail");
                    assert!(error.contains("reject_feedback_checkpoint"), "{error}");
                    // The context RPC returned an ordinary error;
                    // a handler can catch it and keep making calls.
                    ctx.step("after-rpc-error", || async { Ok(()) }).await?;
                } else {
                    response.map_err(absurd::Error::InvalidOptions)?;
                }
                Ok(())
            })
            .await?;
        }
        if scenario == Scenario::FailureAfterResult
            || scenario == Scenario::FailureAfterCheckpointWriteError
        {
            return Err(absurd::Error::InvalidOptions("later step failed".into()));
        }
        if scenario == Scenario::Replay {
            ctx.sleep_for("wait", Duration::from_millis(20)).await?;
        }
        Ok(json!({"done": true}))
    })
    .await
}

#[tokio::test]
async fn durable_feedback_restores_rejection_and_failure_and_preserves_results_on_replay()
-> Result<(), Box<dyn std::error::Error>> {
    let Ok(url) = std::env::var("SESSION_SQLX_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL"))
        .or_else(|_| std::env::var("ABSURD_TEST_DATABASE_URL"))
    else {
        eprintln!("skipping button feedback tests: set SESSION_SQLX_TEST_DATABASE_URL");
        return Ok(());
    };
    let pool = sqlx::PgPool::connect(&url).await?;
    let installed: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname = 'absurd')")
            .fetch_one(&pool)
            .await?;
    if !installed {
        sqlx::raw_sql(include_str!(
            "../../../centaur-session-sqlx/migrations/0007_absurd_workflows.sql"
        ))
        .execute(&pool)
        .await?;
    }
    let client = Client::from_pool_with_options(
        pool,
        ClientOptions {
            queue_name: format!("button_{}", uuid::Uuid::new_v4().simple()),
            ..Default::default()
        },
    )?;
    client.create_queue(None, Default::default()).await?;
    let edits = Arc::new(Mutex::new(Vec::<Value>::new()));
    let handler_calls = Arc::new(AtomicUsize::new(0));
    client.register_task("button", {
        let edits = edits.clone();
        let handler_calls = handler_calls.clone();
        move |input: ScenarioInput, ctx| {
            let edits = edits.clone();
            let handler_calls = handler_calls.clone();
            run_scenario(input, ctx, edits, handler_calls)
        }
    })?;
    let feedback = fixture();
    for scenario in [
        Scenario::Rejection,
        Scenario::Failure,
        Scenario::Result,
        Scenario::Replay,
        Scenario::FailureAfterResult,
        Scenario::ProcessingError,
        Scenario::RestoreError,
        Scenario::CheckpointWriteError,
        Scenario::FailureAfterCheckpointWriteError,
        Scenario::CheckpointReadError,
    ] {
        edits.lock().await.clear();
        handler_calls.store(0, Ordering::SeqCst);
        let params = ScenarioInput {
            feedback: feedback.clone(),
            scenario,
        };
        let spawn = client
            .spawn(
                "button",
                &params,
                SpawnOptions {
                    idempotency_key: Some(format!("{scenario:?}")),
                    max_attempts: Some(if scenario == Scenario::RestoreError {
                        5
                    } else {
                        1
                    }),
                    ..Default::default()
                },
            )
            .await?;
        if scenario.fails_checkpoint_write() {
            // Reject a real checkpoint INSERT after Slack has accepted the
            // result. Only this task's update marker is affected.
            let task_id = uuid::Uuid::parse_str(&spawn.task_id)?;
            sqlx::query(&format!(
                "ALTER TABLE absurd.c_{} DROP CONSTRAINT IF EXISTS reject_feedback_checkpoint,
                 ADD CONSTRAINT reject_feedback_checkpoint
                 CHECK (task_id <> '{task_id}'::uuid OR checkpoint_name <> '$slack_button.updated')",
                client.queue_name()
            )).execute(client.pool()).await?;
        }
        if scenario == Scenario::CheckpointReadError {
            sqlx::query(&format!(
                "INSERT INTO absurd.c_{} (task_id, checkpoint_name, state)
                 VALUES ($1::uuid, '$slack_button.updated', $2::jsonb)",
                client.queue_name()
            ))
            .bind(&spawn.task_id)
            .bind(json!("invalid marker"))
            .execute(client.pool())
            .await?;
        }
        client.work_batch(WorkBatchOptions::default()).await?;
        if scenario == Scenario::Replay {
            tokio::time::sleep(Duration::from_millis(30)).await;
            client.work_batch(WorkBatchOptions::default()).await?;
        }
        let outcome = client
            .fetch_task_result(&spawn.task_id, None)
            .await?
            .unwrap();
        if scenario.fails_handler() {
            assert!(
                matches!(outcome, TaskResultSnapshot::Failed { .. }),
                "{scenario:?}: {outcome:?}"
            );
        } else {
            assert!(
                matches!(outcome, TaskResultSnapshot::Completed { .. }),
                "{scenario:?}: {outcome:?}"
            );
            assert_eq!(
                outcome.result::<Value>()?,
                Some(json!({"done": true})),
                "{scenario:?}"
            );
        }
        let calls = if scenario == Scenario::Replay { 2 } else { 1 };
        assert_eq!(handler_calls.load(Ordering::SeqCst), calls, "{scenario:?}");
        let recorded = edits.lock().await.clone();
        let expected = match scenario {
            Scenario::Rejection | Scenario::Failure => {
                vec![feedback.processing.clone(), feedback.original.clone()]
            }
            Scenario::ProcessingError | Scenario::CheckpointReadError => {
                vec![json!({"channel": "C1", "ts": "1.1", "text": "Approved", "blocks": []})]
            }
            Scenario::RestoreError => vec![feedback.processing.clone()],
            _ => vec![
                feedback.processing.clone(),
                json!({"channel": "C1", "ts": "1.1", "text": "Approved", "blocks": []}),
            ],
        };
        assert_eq!(recorded, expected, "{scenario:?}");
        let duplicate = client
            .spawn(
                "button",
                &params,
                SpawnOptions {
                    idempotency_key: Some(format!("{scenario:?}")),
                    ..Default::default()
                },
            )
            .await?;
        assert!(!duplicate.created);
        assert_eq!(duplicate.task_id, spawn.task_id);
        client.work_batch(WorkBatchOptions::default()).await?;
        assert_eq!(*edits.lock().await, recorded);
        assert_eq!(handler_calls.load(Ordering::SeqCst), calls, "{scenario:?}");
    }
    client.drop_queue(None).await?;
    Ok(())
}
