use std::collections::BTreeMap;
use std::time::Duration;

use centaur_sandbox_agent_k8s::{AgentSandboxBackend, AgentSandboxConfig};
use centaur_sandbox_core::{SandboxBackend, SandboxSpec, SandboxStatus};
use clap::Parser;
use kube::config::KubeConfigOptions;
use kube::{Client, Config};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let kube_config = Config::from_kubeconfig(&KubeConfigOptions {
        context: Some(args.kube_context),
        ..KubeConfigOptions::default()
    })
    .await?;
    let client = Client::try_from(kube_config)?;

    let mut labels = BTreeMap::new();
    labels.insert("centaur.ai/smoke".to_owned(), "agent-sandbox".to_owned());
    let mut config = AgentSandboxConfig::new(args.kube_namespace);
    config.labels = labels;
    config.ready_timeout = Duration::from_secs(90);

    let backend = AgentSandboxBackend::new(client, config);
    let spec = SandboxSpec::new(args.sandbox_image)
        .command(["/bin/sh", "-lc"])
        .args(["sleep 3600"]);

    let handle = backend.create(spec).await?;
    println!("created {}", handle.id.as_str());

    let status = backend.status(&handle.id).await?;
    println!("status after create: {status:?}");
    assert_eq!(status, SandboxStatus::Running);

    backend.pause(&handle.id).await?;
    let status = backend.status(&handle.id).await?;
    println!("status after pause: {status:?}");
    assert!(matches!(
        status,
        SandboxStatus::Suspended | SandboxStatus::Created | SandboxStatus::Running
    ));

    backend.resume(&handle.id).await?;
    let status = backend.status(&handle.id).await?;
    println!("status after resume: {status:?}");
    assert_eq!(status, SandboxStatus::Running);

    backend.stop(&handle.id).await?;
    println!("stopped {}", handle.id.as_str());

    Ok(())
}

#[derive(Debug, Parser)]
#[command(about = "Smoke test the Kubernetes AgentSandbox backend")]
struct Args {
    #[arg(long, env = "KUBE_CONTEXT", default_value = "orbstack")]
    kube_context: String,
    #[arg(long, env = "KUBE_NAMESPACE", default_value = "centaur")]
    kube_namespace: String,
    #[arg(long, env = "SANDBOX_IMAGE", default_value = "busybox:1.36")]
    sandbox_image: String,
}
