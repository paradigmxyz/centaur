mod support;

use test_case::test_case;

#[test_case("local"; "local")]
#[test_case("agent-k8s"; "agent_k8s")]
#[tokio::test]
#[ignore = "requires sandbox e2e infrastructure; run `just e2e-kind`"]
async fn failed_create_cleans_up_observed_resources(implementation_name: &'static str) {
    if let Some(implementation) = support::implementation_if_requested(implementation_name).await {
        support::failed_create_cleans_up_observed_resources(&implementation).await;
    }
}
