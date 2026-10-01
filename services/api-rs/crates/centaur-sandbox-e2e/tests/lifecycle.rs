mod support;

use test_case::test_case;

#[test_case("local"; "local")]
#[test_case("agent-k8s"; "agent_k8s")]
#[tokio::test]
#[ignore = "requires sandbox e2e infrastructure; run `just e2e-kind`"]
async fn create_stop_cleans_up(implementation_name: &'static str) {
    if let Some(implementation) = support::implementation_if_requested(implementation_name).await {
        support::create_stop_cleans_up(&implementation).await;
    }
}

#[tokio::test]
#[ignore = "requires the agent-k8s e2e infrastructure; run `just e2e-kind`"]
async fn stop_preserves_egress_during_pod_termination() {
    if let Some(implementation) = support::implementation_if_requested("agent-k8s").await {
        support::stop_preserves_egress_during_pod_termination(&implementation).await;
    }
}

#[test_case("local"; "local")]
#[test_case("agent-k8s"; "agent_k8s")]
#[tokio::test]
#[ignore = "requires sandbox e2e infrastructure; run `just e2e-kind`"]
async fn pause_resume_restores_running(implementation_name: &'static str) {
    if let Some(implementation) = support::implementation_if_requested(implementation_name).await {
        support::pause_resume_restores_running(&implementation).await;
    }
}

#[tokio::test]
#[ignore = "requires the agent-k8s e2e infrastructure; run `just e2e-kind`"]
async fn resume_reconciles_agent_resources() {
    if let Some(implementation) = support::implementation_if_requested("agent-k8s").await {
        support::resume_reconciles_agent_resources(&implementation).await;
    }
}

#[test_case("local"; "local")]
#[test_case("agent-k8s"; "agent_k8s")]
#[tokio::test]
#[ignore = "requires sandbox e2e infrastructure; run `just e2e-kind`"]
async fn unexpected_shutdown_reports_drift(implementation_name: &'static str) {
    if let Some(implementation) = support::implementation_if_requested(implementation_name).await {
        support::unexpected_shutdown_reports_drift(&implementation).await;
    }
}

#[test_case("local"; "local")]
#[test_case("agent-k8s"; "agent_k8s")]
#[tokio::test]
#[ignore = "requires sandbox e2e infrastructure; run `just e2e-kind`"]
async fn missing_sandbox_operations_are_consistent(implementation_name: &'static str) {
    if let Some(implementation) = support::implementation_if_requested(implementation_name).await {
        support::missing_sandbox_operations_are_consistent(&implementation).await;
    }
}
