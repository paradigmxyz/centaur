use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use centaur_sandbox_agent_k8s::{AgentSandboxBackend, AgentSandboxConfig};
use centaur_sandbox_core::{
    ExecCommand, OutputStream, ReadOptions, ReadResult, SandboxBackend, SandboxId, SandboxSpec,
    SandboxStatus,
};
use centaur_sandbox_local::LocalSandboxBackend;
use centaur_sandbox_manager::{DriftReason, ReconcileOutcome, SandboxManager};
use clap::Parser;
use kube::config::KubeConfigOptions;
use kube::{Client, Config};
use tokio::time::{Instant, sleep};

const ALL_IMPLEMENTATIONS: &[&str] = &["local", "agent-k8s"];

struct SandboxImplementation {
    name: &'static str,
    backend: Arc<dyn SandboxBackend>,
    reconnect_backend: Arc<dyn Fn() -> Arc<dyn SandboxBackend> + Send + Sync>,
    long_running_spec: SandboxSpec,
    short_lived_spec: SandboxSpec,
    byte_io_spec: SandboxSpec,
    invalid_spec: SandboxSpec,
}

#[tokio::test]
#[ignore = "requires sandbox e2e infrastructure; run `just e2e-kind`"]
async fn sandbox_invariants_by_implementation() {
    for implementation in implementations().await {
        eprintln!("running sandbox invariants for {}", implementation.name);
        create_stop_cleans_up(&implementation).await;
        pause_resume_restores_running(&implementation).await;
        unexpected_shutdown_reports_drift(&implementation).await;
        byte_io_round_trips(&implementation).await;
        stdin_close_reaches_eof(&implementation).await;
        reconnect_can_observe_and_stop(&implementation).await;
        pause_blocks_read_write_until_resume(&implementation).await;
        exec_runs_command(&implementation).await;
        missing_sandbox_operations_are_consistent(&implementation).await;
        failed_create_cleans_up_observed_resources(&implementation).await;
    }
}

async fn create_stop_cleans_up(implementation: &SandboxImplementation) {
    let manager = SandboxManager::new(implementation.backend.clone());
    let handle = manager
        .create_running(implementation.long_running_spec.clone())
        .await
        .unwrap_or_else(|err| panic!("{} create failed: {err}", implementation.name));

    eventually_status(&manager, &handle.id, SandboxStatus::Running).await;

    manager
        .stop(&handle.id)
        .await
        .unwrap_or_else(|err| panic!("{} stop failed: {err}", implementation.name));
    eventually_status(&manager, &handle.id, SandboxStatus::Gone).await;
}

async fn pause_resume_restores_running(implementation: &SandboxImplementation) {
    let manager = SandboxManager::new(implementation.backend.clone());
    let handle = manager
        .create_running(implementation.long_running_spec.clone())
        .await
        .unwrap_or_else(|err| panic!("{} create failed: {err}", implementation.name));

    manager
        .pause(&handle.id)
        .await
        .unwrap_or_else(|err| panic!("{} pause failed: {err}", implementation.name));
    eventually_status(&manager, &handle.id, SandboxStatus::Suspended).await;

    manager
        .resume(&handle.id)
        .await
        .unwrap_or_else(|err| panic!("{} resume failed: {err}", implementation.name));
    eventually_status(&manager, &handle.id, SandboxStatus::Running).await;

    manager
        .stop(&handle.id)
        .await
        .unwrap_or_else(|err| panic!("{} stop failed: {err}", implementation.name));
}

async fn unexpected_shutdown_reports_drift(implementation: &SandboxImplementation) {
    let manager = SandboxManager::new(implementation.backend.clone());
    let handle = manager
        .create_running(implementation.short_lived_spec.clone())
        .await
        .unwrap_or_else(|err| panic!("{} create failed: {err}", implementation.name));

    eventually_status(&manager, &handle.id, SandboxStatus::Stopped).await;
    let outcome = manager
        .reconcile_one(&handle.id)
        .await
        .unwrap_or_else(|err| panic!("{} reconcile failed: {err}", implementation.name));

    assert_eq!(
        outcome,
        ReconcileOutcome::Drift(DriftReason::MissingWhileRunning),
        "{} should report drift when a desired-running sandbox exits",
        implementation.name
    );

    manager
        .stop(&handle.id)
        .await
        .unwrap_or_else(|err| panic!("{} cleanup stop failed: {err}", implementation.name));
}

async fn byte_io_round_trips(implementation: &SandboxImplementation) {
    let manager = SandboxManager::new(implementation.backend.clone());
    let handle = manager
        .create_running(implementation.byte_io_spec.clone())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "{} create byte I/O sandbox failed: {err}",
                implementation.name
            )
        });

    let payload = Bytes::from_static(b"byte-io-ping\n");
    let ack = manager
        .write_bytes(&handle.id, payload.clone())
        .await
        .unwrap_or_else(|err| panic!("{} write_bytes failed: {err}", implementation.name));
    assert_eq!(ack.bytes_written, payload.len(), "{}", implementation.name);

    let read = read_stdout(&manager, &handle.id).await;
    assert_eq!(
        read,
        ReadResult::stdout(payload),
        "{} should round-trip bytes through stdin/stdout",
        implementation.name
    );

    manager.stop(&handle.id).await.unwrap();
}

async fn stdin_close_reaches_eof(implementation: &SandboxImplementation) {
    let manager = SandboxManager::new(implementation.backend.clone());
    let handle = manager
        .create_running(implementation.byte_io_spec.clone())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "{} create stdin-close sandbox failed: {err}",
                implementation.name
            )
        });

    manager
        .write_bytes(&handle.id, Bytes::from_static(b"before-close\n"))
        .await
        .unwrap();
    assert_eq!(
        read_stdout(&manager, &handle.id).await,
        ReadResult::stdout(Bytes::from_static(b"before-close\n"))
    );
    manager
        .close_stdin(&handle.id)
        .await
        .unwrap_or_else(|err| panic!("{} close_stdin failed: {err}", implementation.name));
    assert!(
        manager
            .write_bytes(&handle.id, Bytes::from_static(b"after-close\n"))
            .await
            .is_err(),
        "{} should reject writes after stdin close",
        implementation.name
    );

    manager.stop(&handle.id).await.unwrap();
}

async fn reconnect_can_observe_and_stop(implementation: &SandboxImplementation) {
    let manager = SandboxManager::new(implementation.backend.clone());
    let handle = manager
        .create_running(implementation.long_running_spec.clone())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "{} create reconnect sandbox failed: {err}",
                implementation.name
            )
        });

    let reconnected = SandboxManager::new((implementation.reconnect_backend)());
    eventually_status(&reconnected, &handle.id, SandboxStatus::Running).await;
    reconnected
        .stop(&handle.id)
        .await
        .unwrap_or_else(|err| panic!("{} reconnected stop failed: {err}", implementation.name));
    eventually_status(&reconnected, &handle.id, SandboxStatus::Gone).await;
}

async fn pause_blocks_read_write_until_resume(implementation: &SandboxImplementation) {
    let manager = SandboxManager::new(implementation.backend.clone());
    let handle = manager
        .create_running(implementation.byte_io_spec.clone())
        .await
        .unwrap_or_else(|err| {
            panic!(
                "{} create pause I/O sandbox failed: {err}",
                implementation.name
            )
        });

    manager.pause(&handle.id).await.unwrap();
    eventually_status(&manager, &handle.id, SandboxStatus::Suspended).await;

    assert!(
        manager
            .write_bytes(&handle.id, Bytes::from_static(b"paused\n"))
            .await
            .is_err(),
        "{} should reject writes while paused",
        implementation.name
    );
    assert!(
        manager
            .read_bytes(&handle.id, ReadOptions::stdout(64).timeout_ms(10))
            .await
            .is_err(),
        "{} should reject reads while paused",
        implementation.name
    );

    manager.resume(&handle.id).await.unwrap();
    eventually_status(&manager, &handle.id, SandboxStatus::Running).await;
    manager
        .write_bytes(&handle.id, Bytes::from_static(b"after-resume\n"))
        .await
        .unwrap();
    assert_eq!(
        read_stdout(&manager, &handle.id).await,
        ReadResult::stdout(Bytes::from_static(b"after-resume\n"))
    );
    manager.stop(&handle.id).await.unwrap();
}

async fn missing_sandbox_operations_are_consistent(implementation: &SandboxImplementation) {
    let manager = SandboxManager::new(implementation.backend.clone());
    let missing = SandboxId::new(format!("missing-{}", implementation.name));

    assert_eq!(
        manager
            .status(&missing)
            .await
            .unwrap_or(SandboxStatus::Gone),
        SandboxStatus::Gone
    );
    assert!(
        manager.pause(&missing).await.is_err(),
        "{} should not pause a missing sandbox",
        implementation.name
    );
    assert!(
        manager.resume(&missing).await.is_err(),
        "{} should not resume a missing sandbox",
        implementation.name
    );
    manager.stop(&missing).await.unwrap_or_else(|err| {
        panic!(
            "{} stop should be idempotent for missing sandboxes: {err}",
            implementation.name
        )
    });
}

async fn exec_runs_command(implementation: &SandboxImplementation) {
    let manager = SandboxManager::new(implementation.backend.clone());
    let handle = manager
        .create_running(implementation.long_running_spec.clone())
        .await
        .unwrap_or_else(|err| panic!("{} create exec sandbox failed: {err}", implementation.name));

    let result = manager
        .exec(
            &handle.id,
            ExecCommand::new(["/bin/sh", "-lc", "printf exec-ok"]),
        )
        .await
        .unwrap_or_else(|err| panic!("{} exec failed: {err}", implementation.name));

    assert!(
        result.success(),
        "{} exec should succeed",
        implementation.name
    );
    assert_eq!(result.stdout, b"exec-ok", "{}", implementation.name);

    manager.stop(&handle.id).await.unwrap();
}

async fn failed_create_cleans_up_observed_resources(implementation: &SandboxImplementation) {
    let before = implementation
        .backend
        .list_observed()
        .await
        .unwrap_or_default()
        .len();
    assert!(
        implementation
            .backend
            .create(implementation.invalid_spec.clone())
            .await
            .is_err(),
        "{} invalid create should fail",
        implementation.name
    );
    eventually_observed_count_at_most(implementation.backend.clone(), before).await;
}

async fn eventually_status<S>(manager: &SandboxManager<S>, id: &SandboxId, expected: SandboxStatus)
where
    S: centaur_sandbox_manager::DesiredStateStore,
{
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut latest;
    loop {
        let status = manager.status(id).await.unwrap_or(SandboxStatus::Gone);
        if status == expected {
            return;
        }
        latest = Some(status);
        assert!(
            Instant::now() < deadline,
            "sandbox {} did not reach {expected:?}; latest status: {:?}",
            id.as_str(),
            latest
        );
        sleep(Duration::from_millis(250)).await;
    }
}

async fn eventually_observed_count_at_most(backend: Arc<dyn SandboxBackend>, expected_max: usize) {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let count = backend.list_observed().await.unwrap_or_default().len();
        if count <= expected_max {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "observed sandbox count stayed above {expected_max}; latest count: {count}"
        );
        sleep(Duration::from_millis(250)).await;
    }
}

async fn read_stdout<S>(manager: &SandboxManager<S>, id: &SandboxId) -> ReadResult
where
    S: centaur_sandbox_manager::DesiredStateStore,
{
    manager
        .read_bytes(
            id,
            ReadOptions {
                stream: OutputStream::Stdout,
                after_offset: None,
                max_bytes: 1024,
                timeout_ms: Some(5_000),
            },
        )
        .await
        .unwrap_or_else(|err| panic!("read stdout failed for {}: {err}", id.as_str()))
}

async fn implementations() -> Vec<SandboxImplementation> {
    let args = E2eArgs::from_env();
    let requested = args.sandbox_e2e_impls.as_str();
    let names = if requested.trim() == "all" {
        ALL_IMPLEMENTATIONS.to_vec()
    } else {
        requested
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>()
    };

    let mut implementations = Vec::new();
    for name in names {
        match name {
            "local" => implementations.push(local_implementation()),
            "agent-k8s" => implementations.push(agent_k8s_implementation().await),
            other => panic!("unknown sandbox e2e implementation {other:?}"),
        }
    }
    implementations
}

fn local_implementation() -> SandboxImplementation {
    let backend = Arc::new(LocalSandboxBackend::new());
    let reconnect_backend = backend.clone();
    SandboxImplementation {
        name: "local",
        backend,
        reconnect_backend: Arc::new(move || reconnect_backend.clone()),
        long_running_spec: shell_spec("sleep 3600"),
        short_lived_spec: shell_spec("sleep 0.02"),
        byte_io_spec: SandboxSpec::new("/bin/cat"),
        invalid_spec: SandboxSpec::new("/definitely-not-a-centaur-command"),
    }
}

async fn agent_k8s_implementation() -> SandboxImplementation {
    let args = E2eArgs::from_env();
    let context = args
        .sandbox_e2e_k8s_context
        .or(args.kube_context)
        .unwrap_or_else(|| "kind-centaur-api-rs-e2e".to_owned());
    let namespace = args
        .sandbox_e2e_k8s_namespace
        .or(args.kube_namespace)
        .unwrap_or_else(|| "centaur-sandbox-e2e".to_owned());
    let image = args.sandbox_e2e_k8s_image;

    let kube_config = Config::from_kubeconfig(&KubeConfigOptions {
        context: Some(context),
        ..KubeConfigOptions::default()
    })
    .await
    .expect("load e2e kube config");
    let client = Client::try_from(kube_config).expect("create e2e kube client");
    let mut config = AgentSandboxConfig::new(namespace);
    config.ready_timeout = Duration::from_secs(90);
    let backend = Arc::new(AgentSandboxBackend::new(client.clone(), config.clone()));

    SandboxImplementation {
        name: "agent-k8s",
        backend,
        reconnect_backend: Arc::new(move || {
            Arc::new(AgentSandboxBackend::new(client.clone(), config.clone()))
        }),
        long_running_spec: k8s_shell_spec(&image, "sleep 3600"),
        short_lived_spec: k8s_shell_spec(&image, "sleep 1"),
        byte_io_spec: k8s_shell_spec(&image, "cat"),
        invalid_spec: SandboxSpec::new(image).command(["/definitely-not-a-centaur-command"]),
    }
}

#[derive(Debug, Parser)]
struct E2eArgs {
    #[arg(long, env = "SANDBOX_E2E_IMPLS", default_value = "all")]
    sandbox_e2e_impls: String,
    #[arg(long, env = "SANDBOX_E2E_K8S_CONTEXT")]
    sandbox_e2e_k8s_context: Option<String>,
    #[arg(long, env = "KUBE_CONTEXT")]
    kube_context: Option<String>,
    #[arg(long, env = "SANDBOX_E2E_K8S_NAMESPACE")]
    sandbox_e2e_k8s_namespace: Option<String>,
    #[arg(long, env = "KUBE_NAMESPACE")]
    kube_namespace: Option<String>,
    #[arg(long, env = "SANDBOX_E2E_K8S_IMAGE", default_value = "busybox:1.36")]
    sandbox_e2e_k8s_image: String,
}

impl E2eArgs {
    fn from_env() -> Self {
        Self::parse_from(["centaur-sandbox-e2e"])
    }
}

fn shell_spec(script: &str) -> SandboxSpec {
    SandboxSpec::new("/bin/sh")
        .command(["/bin/sh", "-lc"])
        .args([script])
}

fn k8s_shell_spec(image: &str, script: &str) -> SandboxSpec {
    SandboxSpec::new(image)
        .command(["/bin/sh", "-lc"])
        .args([script])
}
