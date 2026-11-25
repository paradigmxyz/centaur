use std::time::{Duration, SystemTime, UNIX_EPOCH};

use absurd::{
    AwaitEventOptions, AwaitTaskResultOptions, CancellationPolicy, Client, ClientOptions,
    CreateQueueOptions, Error, Result, SpawnOptions, TaskContext, TaskRegistrationOptions,
    TaskResultSnapshot, WorkerOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MissionParams {
    mission_id: String,
    destination: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    revision: i32,
    parts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChildHandle {
    part: String,
    task_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FabricationParams {
    mission_id: String,
    part: String,
    revision: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FabricationResult {
    part: String,
    serial: String,
    quality_score: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LaunchApproval {
    approved_by: String,
    risk_budget: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MissionResult {
    mission_id: String,
    destination: String,
    revision: i32,
    parts: Vec<FabricationResult>,
    approved_by: String,
    countdown_ms: u64,
    status: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after epoch")
        .as_millis();
    let parent_queue = format!("crazy_parent_{suffix}");
    let child_queue = format!("crazy_child_{suffix}");
    let mission_id = format!("absurd-{suffix}");

    let parent = Client::connect(ClientOptions {
        queue_name: parent_queue.clone(),
        ..ClientOptions::default()
    })
    .await?;
    let child = Client::from_pool_with_options(
        parent.pool().clone(),
        ClientOptions {
            queue_name: child_queue.clone(),
            ..ClientOptions::default()
        },
    )?;

    parent
        .create_queue(Some(&parent_queue), CreateQueueOptions::default())
        .await?;
    child
        .create_queue(Some(&child_queue), CreateQueueOptions::default())
        .await?;

    register_child_worker(&child)?;
    register_parent_workflow(&parent, child.clone())?;

    let parent_worker = parent.start_worker(WorkerOptions {
        worker_id: Some("rust-crazy-parent".to_string()),
        concurrency: 2,
        poll_interval: Duration::from_millis(50),
        fatal_on_lease_timeout: false,
        ..WorkerOptions::default()
    });
    let child_worker = child.start_worker(WorkerOptions {
        worker_id: Some("rust-crazy-child".to_string()),
        concurrency: 4,
        poll_interval: Duration::from_millis(50),
        fatal_on_lease_timeout: false,
        ..WorkerOptions::default()
    });

    println!("queues: parent={parent_queue} child={child_queue}");

    let spawned = parent
        .spawn(
            "mission-control",
            MissionParams {
                mission_id: mission_id.clone(),
                destination: "Europa subsurface archive".to_string(),
            },
            SpawnOptions {
                idempotency_key: Some(format!("mission:{mission_id}")),
                ..SpawnOptions::default()
            },
        )
        .await?;

    println!(
        "spawned mission task={} run={} attempt={}",
        spawned.task_id, spawned.run_id, spawned.attempt
    );

    let approval_client = parent.clone();
    let approval_event = format!("launch.approval:{mission_id}");
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(700)).await;
        println!("external signal: launch approval event emitted");
        approval_client
            .emit_event(
                &approval_event,
                LaunchApproval {
                    approved_by: "orbital-review-board".to_string(),
                    risk_budget: "spicy-but-approved".to_string(),
                },
                None,
            )
            .await
            .expect("emit approval event");
    });

    let snapshot = parent
        .await_task_result(&spawned.task_id, None, Some(Duration::from_secs(20)))
        .await?;

    parent_worker.close().await?;
    child_worker.close().await?;

    match snapshot {
        TaskResultSnapshot::Completed { result } => {
            println!("final mission payload:");
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        other => Err(Error::InvalidOptions(format!(
            "mission ended without success: {other:?}"
        ))),
    }
}

fn register_child_worker(child: &Client) -> Result<()> {
    child.register_task_with(
        TaskRegistrationOptions {
            name: "fabricate-part".to_string(),
            default_max_attempts: Some(2),
            ..TaskRegistrationOptions::new("fabricate-part")
        },
        |params: FabricationParams, ctx| async move {
            println!(
                "child task: fabricating {} attempt {}",
                params.part,
                ctx.attempt()
            );

            if params.part == "heat-shield" && ctx.attempt() == 1 {
                return Err(Error::InvalidOptions(
                    "laser cutter overheated; retrying from durable run log".to_string(),
                ));
            }

            let serial = {
                let mission_id = params.mission_id.clone();
                let part = params.part.clone();
                let attempt = ctx.attempt();
                ctx.step("mint-serial", || async move {
                    Ok(format!(
                        "{}-{}-r{}-a{}",
                        mission_id,
                        part.replace(' ', "-"),
                        params.revision,
                        attempt
                    ))
                })
                .await?
            };

            ctx.step("quality-check", || async move {
                Ok(FabricationResult {
                    part: params.part,
                    serial,
                    quality_score: 99,
                })
            })
            .await
        },
    )
}

fn register_parent_workflow(parent: &Client, child: Client) -> Result<()> {
    parent.register_task_with(
        TaskRegistrationOptions {
            name: "mission-control".to_string(),
            default_max_attempts: Some(3),
            default_cancellation: Some(CancellationPolicy {
                max_duration: Some(60),
                max_delay: Some(30),
            }),
            ..TaskRegistrationOptions::new("mission-control")
        },
        move |params: MissionParams, ctx: TaskContext| {
            let child = child.clone();
            async move {
                let manifest: Manifest = ctx
                    .step("decode-briefing", || async {
                        println!("parent task: decoding encrypted mission briefing");
                        Ok(Manifest {
                            revision: 7,
                            parts: vec![
                                "ion-drive".to_string(),
                                "ice-radar".to_string(),
                                "heat-shield".to_string(),
                            ],
                        })
                    })
                    .await?;

                let child_handles: Vec<ChildHandle> = {
                    let child = child.clone();
                    let mission_id = params.mission_id.clone();
                    let parts = manifest.parts.clone();
                    let revision = manifest.revision;
                    ctx.step("spawn-fabrication", || async move {
                        println!("parent task: spawning child fabrication tasks");
                        let mut handles = Vec::new();
                        for part in parts {
                            let spawned = child
                                .spawn(
                                    "fabricate-part",
                                    FabricationParams {
                                        mission_id: mission_id.clone(),
                                        part: part.clone(),
                                        revision,
                                    },
                                    SpawnOptions {
                                        idempotency_key: Some(format!(
                                            "fabricate:{mission_id}:{part}:r{revision}"
                                        )),
                                        ..SpawnOptions::default()
                                    },
                                )
                                .await?;
                            handles.push(ChildHandle {
                                part,
                                task_id: spawned.task_id,
                            });
                        }
                        Ok(handles)
                    })
                    .await?
                };

                let mut fabricated = Vec::new();
                for child_task in child_handles {
                    let snapshot = ctx
                        .await_task_result(
                            &child_task.task_id,
                            AwaitTaskResultOptions {
                                queue: Some(child.queue_name().to_string()),
                                timeout: Some(Duration::from_secs(10)),
                                step_name: Some(format!("await-child:{}", child_task.part)),
                            },
                        )
                        .await?;
                    let part = match snapshot {
                        TaskResultSnapshot::Completed { result } => {
                            serde_json::from_value::<FabricationResult>(result)?
                        }
                        TaskResultSnapshot::Failed { failure } => {
                            return Err(Error::InvalidOptions(format!(
                                "child {} failed: {failure}",
                                child_task.part
                            )));
                        }
                        other => {
                            return Err(Error::InvalidOptions(format!(
                                "child {} ended unexpectedly: {other:?}",
                                child_task.part
                            )));
                        }
                    };
                    fabricated.push(part);
                }

                let assembled: Vec<FabricationResult> = ctx
                    .step("assemble-parts", || async move {
                        println!("parent task: assembling fabricated parts");
                        Ok(fabricated)
                    })
                    .await?;

                let approval: LaunchApproval = ctx
                    .await_event(
                        &format!("launch.approval:{}", params.mission_id),
                        AwaitEventOptions {
                            timeout: Some(Duration::from_secs(5)),
                            ..AwaitEventOptions::default()
                        },
                    )
                    .await?;

                println!(
                    "parent task: got approval from {} with risk budget {}",
                    approval.approved_by, approval.risk_budget
                );

                ctx.sleep_for("pre-launch-countdown", Duration::from_millis(250))
                    .await?;

                ctx.step("commit-launch", || async move {
                    println!("parent task: committing final mission payload");
                    Ok(json!(MissionResult {
                        mission_id: params.mission_id,
                        destination: params.destination,
                        revision: manifest.revision,
                        parts: assembled,
                        approved_by: approval.approved_by,
                        countdown_ms: 250,
                        status: "launched".to_string(),
                    }))
                })
                .await
            }
        },
    )
}
