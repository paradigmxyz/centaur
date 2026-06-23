#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use centaur_sandbox_agent_k8s::{AgentSandboxBackend, AgentSandboxConfig};
use centaur_sandbox_core::{SandboxBackend, SandboxId, SandboxSpec, SandboxStatus, SandboxWrite};
use centaur_sandbox_local::LocalSandboxBackend;
use centaur_sandbox_manager::{DriftReason, ReconcileOutcome, SandboxManager};
use clap::Parser;
use kube::config::KubeConfigOptions;
use kube::{Client, Config};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{interval, timeout};

const ALL_IMPLEMENTATIONS: &[&str] = &["local", "agent-k8s"];

pub(crate) struct SandboxImplementation {
    name: &'static str,
    backend: Arc<dyn SandboxBackend>,
    reconnect_backend: Arc<dyn Fn() -> Arc<dyn SandboxBackend> + Send + Sync>,
    long_running_spec: SandboxSpec,
    short_lived_spec: SandboxSpec,
    byte_io_spec: SandboxSpec,
    invalid_spec: SandboxSpec,
}

pub(crate) async fn implementation_if_requested(
    name: &'static str,
) -> Option<SandboxImplementation> {
    let args = E2eArgs::from_env();
    validate_requested_implementations(&args);
    if !args.includes_implementation(name) {
        return None;
    }

    Some(match name {
        "local" => local_implementation(),
        "agent-k8s" => agent_k8s_implementation().await,
        other => panic!("unknown sandbox e2e implementation {other:?}"),
    })
}

pub(crate) async fn create_stop_cleans_up(implementation: &SandboxImplementation) {
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

pub(crate) async fn pause_resume_restores_running(implementation: &SandboxImplementation) {
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

pub(crate) async fn unexpected_shutdown_reports_drift(implementation: &SandboxImplementation) {
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

pub(crate) async fn byte_io_round_trips(implementation: &SandboxImplementation) {
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

    let mut io = manager
        .open_io(&handle.id)
        .await
        .unwrap_or_else(|err| panic!("{} open_io failed: {err}", implementation.name))
        .into_parts();
    let payload = b"byte-io-ping\n";
    io.stdin
        .write_all(payload)
        .await
        .unwrap_or_else(|err| panic!("{} stdin write failed: {err}", implementation.name));
    io.stdin
        .flush()
        .await
        .unwrap_or_else(|err| panic!("{} stdin flush failed: {err}", implementation.name));

    let read = read_stdout(&mut io.stdout, payload.len()).await;
    assert_eq!(
        read, payload,
        "{} should round-trip bytes through stdin/stdout",
        implementation.name
    );

    manager.stop(&handle.id).await.unwrap();
}

pub(crate) async fn stdin_drop_closes_write_half(implementation: &SandboxImplementation) {
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

    let io = manager
        .open_io(&handle.id)
        .await
        .unwrap_or_else(|err| panic!("{} open_io failed: {err}", implementation.name))
        .into_parts();
    let mut stdin = io.stdin;
    let mut stdout = io.stdout;
    let _guard = io.guard;

    stdin.write_all(b"before-close\n").await.unwrap();
    stdin.flush().await.unwrap();
    assert_eq!(
        read_stdout(&mut stdout, b"before-close\n".len()).await,
        b"before-close\n"
    );
    drop_stdin(stdin);

    manager.stop(&handle.id).await.unwrap();
}

pub(crate) async fn reconnect_can_observe_and_stop(implementation: &SandboxImplementation) {
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

pub(crate) async fn pause_blocks_read_write_until_resume(implementation: &SandboxImplementation) {
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
        manager.open_io(&handle.id).await.is_err(),
        "{} should reject opening I/O while paused",
        implementation.name
    );

    manager.resume(&handle.id).await.unwrap();
    eventually_status(&manager, &handle.id, SandboxStatus::Running).await;
    let mut io = manager.open_io(&handle.id).await.unwrap().into_parts();
    io.stdin.write_all(b"after-resume\n").await.unwrap();
    io.stdin.flush().await.unwrap();
    assert_eq!(
        read_stdout(&mut io.stdout, b"after-resume\n".len()).await,
        b"after-resume\n"
    );
    manager.stop(&handle.id).await.unwrap();
}

pub(crate) async fn missing_sandbox_operations_are_consistent(
    implementation: &SandboxImplementation,
) {
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
    assert!(
        manager.open_io(&missing).await.is_err(),
        "{} should not open I/O for a missing sandbox",
        implementation.name
    );
    manager.stop(&missing).await.unwrap_or_else(|err| {
        panic!(
            "{} stop should be idempotent for missing sandboxes: {err}",
            implementation.name
        )
    });
}

pub(crate) async fn failed_create_cleans_up_observed_resources(
    implementation: &SandboxImplementation,
) {
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
    let mut latest = SandboxStatus::Gone;
    let result = timeout(Duration::from_secs(45), async {
        let mut ticks = interval(Duration::from_millis(250));
        loop {
            let status = manager.status(id).await.unwrap_or(SandboxStatus::Gone);
            if status == expected {
                return;
            }
            latest = status;
            ticks.tick().await;
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "sandbox {} did not reach {expected:?}; latest status: {latest:?}",
        id.as_str()
    );
}

async fn eventually_observed_count_at_most(backend: Arc<dyn SandboxBackend>, expected_max: usize) {
    let mut latest = usize::MAX;
    let result = timeout(Duration::from_secs(45), async {
        let mut ticks = interval(Duration::from_millis(250));
        loop {
            let count = backend.list_observed().await.unwrap_or_default().len();
            if count <= expected_max {
                return;
            }
            latest = count;
            ticks.tick().await;
        }
    })
    .await;

    assert!(
        result.is_ok(),
        "observed sandbox count stayed above {expected_max}; latest count: {latest}"
    );
}

async fn read_stdout(stdout: &mut centaur_sandbox_core::SandboxRead, len: usize) -> Vec<u8> {
    let mut buf = vec![0; len];
    timeout(Duration::from_secs(5), stdout.read_exact(&mut buf))
        .await
        .expect("read stdout timed out")
        .expect("read stdout failed");
    buf
}

fn drop_stdin(stdin: SandboxWrite) {
    drop(stdin);
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

fn validate_requested_implementations(args: &E2eArgs) {
    if args.sandbox_e2e_impls.trim() == "all" {
        return;
    }
    for name in args.requested_implementation_names() {
        assert!(
            ALL_IMPLEMENTATIONS.contains(&name),
            "unknown sandbox e2e implementation {name:?}"
        );
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

    fn includes_implementation(&self, name: &str) -> bool {
        if self.sandbox_e2e_impls.trim() == "all" {
            return true;
        }
        self.requested_implementation_names()
            .into_iter()
            .any(|requested| requested == name)
    }

    fn requested_implementation_names(&self) -> Vec<&str> {
        self.sandbox_e2e_impls
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect()
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
