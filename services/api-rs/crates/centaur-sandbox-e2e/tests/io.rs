mod support;

use test_case::test_case;

#[test_case("local"; "local")]
#[test_case("agent-k8s"; "agent_k8s")]
#[tokio::test]
#[ignore = "requires sandbox e2e infrastructure; run `just e2e-kind`"]
async fn byte_io_round_trips(implementation_name: &'static str) {
    if let Some(implementation) = support::implementation_if_requested(implementation_name).await {
        support::byte_io_round_trips(&implementation).await;
    }
}

#[test_case("local"; "local")]
#[test_case("agent-k8s"; "agent_k8s")]
#[tokio::test]
#[ignore = "requires sandbox e2e infrastructure; run `just e2e-kind`"]
async fn stdin_drop_closes_write_half(implementation_name: &'static str) {
    if let Some(implementation) = support::implementation_if_requested(implementation_name).await {
        support::stdin_drop_closes_write_half(&implementation).await;
    }
}

#[test_case("local"; "local")]
#[test_case("agent-k8s"; "agent_k8s")]
#[tokio::test]
#[ignore = "requires sandbox e2e infrastructure; run `just e2e-kind`"]
async fn pause_blocks_read_write_until_resume(implementation_name: &'static str) {
    if let Some(implementation) = support::implementation_if_requested(implementation_name).await {
        support::pause_blocks_read_write_until_resume(&implementation).await;
    }
}
