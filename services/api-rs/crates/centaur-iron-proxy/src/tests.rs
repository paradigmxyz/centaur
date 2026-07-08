use super::*;

#[test]
fn harness_auth_fragments_are_baked_in() {
    let codex = harness_auth_fragment("codex", "api_key").unwrap().unwrap();
    let codex_placeholders = placeholder_env(&[codex]);
    assert_eq!(
        codex_placeholders.get("OPENAI_API_KEY").map(String::as_str),
        Some("OPENAI_API_KEY")
    );

    // access_token carries the token-broker credential, not a replace
    // placeholder, so it contributes no sandbox placeholder env.
    let codex_access = harness_auth_fragment("codex", "access_token")
        .unwrap()
        .unwrap();
    assert!(placeholder_env(&[codex_access]).is_empty());

    let openrouter = harness_auth_fragment("openrouter", "api_key")
        .unwrap()
        .unwrap();
    let openrouter_placeholders = placeholder_env(&[openrouter]);
    assert_eq!(
        openrouter_placeholders
            .get("OPENROUTER_API_KEY")
            .map(String::as_str),
        Some("OPENROUTER_API_KEY")
    );

    let meta_ai = harness_auth_fragment("meta-ai", "api_key")
        .unwrap()
        .unwrap();
    let meta_ai_placeholders = placeholder_env(&[meta_ai]);
    assert_eq!(
        meta_ai_placeholders
            .get("META_AI_API_KEY")
            .map(String::as_str),
        Some("META_AI_API_KEY")
    );

    let claude_code = harness_auth_fragment("claude-code", "api_key")
        .unwrap()
        .unwrap();
    let claude_code_placeholders = placeholder_env(&[claude_code]);
    assert_eq!(
        claude_code_placeholders
            .get("ANTHROPIC_API_KEY")
            .map(String::as_str),
        Some("ANTHROPIC_API_KEY")
    );

    assert!(harness_auth_fragment("codex", "bogus").unwrap().is_none());

    let infra = infra_fragment().unwrap();
    assert_eq!(
        infra.top_level["proxy"]["upstream_response_header_timeout"].as_str(),
        Some("120s")
    );
    let placeholders = placeholder_env(&[infra]);
    for name in ["GITHUB_TOKEN", "SLACK_BOT_TOKEN"] {
        assert_eq!(placeholders.get(name).map(String::as_str), Some(name));
    }
}

#[test]
fn pg_sandbox_dsns_reads_name_and_database_from_fragments() {
    // A listener with a sandbox_env (the api-rs-internal annotation api-rs
    // stamps from each tool's declared pg_dsn name/database) is surfaced; a
    // listener without one (proxy-only) is skipped. Results are sorted/deduped.
    let fragment = load_fragment_str(
        r#"
postgres:
  - name: reshift_dsn
    sandbox_env:
      name: RESHIFT_DSN
      database: warehouse
  - name: analytics_dsn
    sandbox_env:
      name: ANALYTICS_DSN
      database: analytics
  - name: proxy_only
"#,
    )
    .unwrap();

    let dsns = pg_sandbox_dsns(&[fragment.clone(), fragment]);
    assert_eq!(
        dsns,
        vec![
            ("ANALYTICS_DSN".to_owned(), "analytics".to_owned()),
            ("RESHIFT_DSN".to_owned(), "warehouse".to_owned()),
        ]
    );
}

#[test]
fn access_token_fragment_carries_no_broker_credentials_block() {
    // Broker credentials now live in iron-control, not the proxy fragment. The
    // access-token fragment still references the credential via a token_broker
    // source, but the unknown `broker_credentials:` key (if any) is ignored.
    let codex = harness_auth_fragment("codex", "access_token")
        .unwrap()
        .unwrap();
    assert!(!codex.top_level.contains_key("broker_credentials"));
}

#[test]
fn shipped_proxy_allowlist_preserves_railway_project_tokens() {
    let config: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../../../../iron-proxy/iron-proxy.yaml")).unwrap();
    let transforms = config["transforms"].as_sequence().unwrap();
    let header_allowlist = transforms
        .iter()
        .find(|transform| transform["name"].as_str() == Some("header_allowlist"))
        .unwrap();
    let headers = header_allowlist["config"]["headers"].as_sequence().unwrap();

    assert!(
        headers
            .iter()
            .any(|header| header.as_str() == Some("project-access-token"))
    );
}
