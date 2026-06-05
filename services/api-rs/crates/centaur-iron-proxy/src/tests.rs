use std::collections::BTreeMap;

use super::*;

#[test]
fn harness_auth_fragments_are_baked_in() {
    let codex = harness_auth_fragment("codex", "api_key").unwrap().unwrap();
    assert!(placeholder_env(&[codex]).is_empty());

    // access_token carries the token-broker credential, not a replace
    // placeholder, so it contributes no sandbox placeholder env.
    let codex_access = harness_auth_fragment("codex", "access_token")
        .unwrap()
        .unwrap();
    assert!(placeholder_env(&[codex_access]).is_empty());

    assert!(harness_auth_fragment("codex", "bogus").unwrap().is_none());

    let infra = infra_fragment().unwrap();
    let placeholders = placeholder_env(&[infra]);
    for name in ["AMP_API_KEY", "GITHUB_TOKEN", "SLACK_BOT_TOKEN"] {
        assert_eq!(placeholders.get(name).map(String::as_str), Some(name));
    }
}

#[test]
fn access_token_fragment_carries_no_broker_credentials_block() {
    // Broker credentials now live in iron-control, not the proxy fragment. The
    // access-token fragment still references the credential via a token_broker
    // source, but the unknown `broker_credentials:` key (if any) is ignored.
    let codex = harness_auth_fragment("codex", "access_token").unwrap().unwrap();
    assert!(!codex.top_level.contains_key("broker_credentials"));
}
