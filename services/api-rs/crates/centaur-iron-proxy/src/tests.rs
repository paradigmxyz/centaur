use std::collections::BTreeMap;

use serde_yaml::Value;

use super::*;

fn fragment(yaml: &str) -> ProxyFragment {
    load_fragment_str(yaml).unwrap()
}

#[test]
fn harness_auth_fragments_are_baked_in() {
    let codex = harness_auth_fragment("codex", "api_key").unwrap().unwrap();
    assert_eq!(
        placeholder_env(&[codex]),
        BTreeMap::from([("OPENAI_API_KEY".to_owned(), "OPENAI_API_KEY".to_owned())])
    );

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
fn renders_token_broker_config() {
    let mut fragments = vec![
        harness_auth_fragment("codex", "access_token")
            .unwrap()
            .unwrap(),
        harness_auth_fragment("claude-code", "access_token")
            .unwrap()
            .unwrap(),
    ];
    fragments.push(fragment(
        r#"
broker_credentials:
  - id: okta
    token_endpoint: https://idp.example.com/oauth/token
    client_id: { placeholder: OKTA_BUNDLE, json_key: client_id }
    client_secret: { placeholder: OKTA_BUNDLE, json_key: client_secret }
    store: { placeholder: OKTA_BLOB }
  - id: openai-codex
    token_endpoint: https://duplicate.example.com/token
    client_id: { placeholder: DUPLICATE_CLIENT_ID }
    store: { placeholder: DUPLICATE_BLOB }
"#,
    ));

    let rendered = render_token_broker_yaml_with_source_policy(
        &fragments,
        &SourcePolicy::onepassword_connect("prod-agents", "10m"),
    )
    .unwrap();
    let cfg: Value = serde_yaml::from_str(&rendered).unwrap();
    let credentials = cfg["credentials"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|credential| (credential["id"].as_str().unwrap(), credential))
        .collect::<BTreeMap<_, _>>();

    assert_eq!(cfg["bearer_auth_env"], BROKER_BEARER_AUTH_ENV);
    assert_eq!(
        credentials.keys().copied().collect::<Vec<_>>(),
        vec!["anthropic-claude", "okta", "openai-codex"]
    );
    assert_eq!(
        credentials["okta"]["client_id"]["secret_ref"],
        "op://prod-agents/OKTA_BUNDLE/credential"
    );
    assert_eq!(
        credentials["openai-codex"]["token_endpoint"],
        "https://auth.openai.com/oauth/token"
    );
    assert!(!rendered.contains("placeholder:"));
}

#[test]
fn rejects_invalid_token_broker_store_sources() {
    let env_store = fragment(
        r#"
broker_credentials:
  - id: openai-codex
    token_endpoint: https://auth.openai.com/oauth/token
    client_id: { placeholder: OPENAI_CODEX_CLIENT_ID }
    store: { placeholder: OPENAI_CODEX_BLOB }
"#,
    );
    let err = render_token_broker_yaml_with_source_policy(&[env_store], &SourcePolicy::env())
        .unwrap_err();
    assert!(
        matches!(err, IronProxyConfigError::BrokerStoreEnv { placeholder } if placeholder == "OPENAI_CODEX_BLOB")
    );

    let json_key_store = fragment(
        r#"
broker_credentials:
  - id: openai-codex
    token_endpoint: https://auth.openai.com/oauth/token
    client_id: { placeholder: OPENAI_CODEX_CLIENT_ID }
    store: { placeholder: OPENAI_CODEX_BUNDLE, json_key: refresh_token }
"#,
    );
    let err = render_token_broker_yaml_with_source_policy(
        &[json_key_store],
        &SourcePolicy::onepassword("ai-agents", "10m"),
    )
    .unwrap_err();
    assert!(
        matches!(err, IronProxyConfigError::BrokerStoreJsonKey { placeholder } if placeholder == "OPENAI_CODEX_BUNDLE")
    );
}
