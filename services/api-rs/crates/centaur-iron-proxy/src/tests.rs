use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_yaml::Value;

use super::*;

fn fragment(yaml: &str) -> ProxyFragment {
    load_fragment_str(yaml).unwrap()
}

fn render_cfg(fragments: &[ProxyFragment], policy: SourcePolicy) -> (String, Value) {
    let rendered = render_proxy_yaml_with_source_policy(None, fragments, &policy).unwrap();
    let cfg = serde_yaml::from_str(&rendered).unwrap();
    (rendered, cfg)
}

fn transform_names(cfg: &Value) -> Vec<&str> {
    cfg["transforms"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|value| value["name"].as_str().unwrap())
        .collect()
}

fn temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "centaur-iron-proxy-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn renders_managed_proxy_config() {
    let fragment = fragment(
        r#"
mcp:
  servers:
    - name: github
      auth: { placeholder: GITHUB_TOKEN }
transforms:
  - name: secrets
    config:
      secrets:
        - replace:
            proxy_value: OPENAI_API_KEY
            match_headers: ["Authorization"]
          rules: [{ host: api.openai.com }]
  - name: oauth_token
    config:
      tokens:
        - grant: refresh_token
          client_id: { placeholder: GOOGLE_OAUTH_JSON, json_key: client_id }
          client_secret: { placeholder: GOOGLE_OAUTH_JSON, json_key: client_secret }
          refresh_token: { placeholder: GOOGLE_REFRESH_TOKEN }
          token_endpoint: https://oauth2.googleapis.com/token
          rules: [{ host: gmail.googleapis.com }]
postgres:
  - name: warehouse
    listen: 0.0.0.0:5440
    upstream:
      dsn: { placeholder: WAREHOUSE_DSN_UPSTREAM }
    client:
      user: app_user
      password_env: PG_PROXY_PASSWORD_WAREHOUSE
    sandbox_env: { name: WAREHOUSE_DSN, database: warehouse }
"#,
    );

    assert_eq!(
        pg_dsn_envs(std::slice::from_ref(&fragment)),
        vec![PgDsnEnv {
            env_name: "WAREHOUSE_DSN".to_owned(),
            database: "warehouse".to_owned(),
            port: 5440,
            user: "app_user".to_owned(),
            password_env: "PG_PROXY_PASSWORD_WAREHOUSE".to_owned(),
        }]
    );
    assert_eq!(
        placeholder_env(std::slice::from_ref(&fragment)),
        BTreeMap::from([("OPENAI_API_KEY".to_owned(), "OPENAI_API_KEY".to_owned())])
    );

    let (rendered, cfg) = render_cfg(&[fragment], SourcePolicy::onepassword("ai-agents", "10m"));
    assert_eq!(
        transform_names(&cfg),
        vec!["allowlist", "secrets", "oauth_token", "header_allowlist"]
    );
    assert_eq!(
        cfg["mcp"]["servers"][0]["auth"]["secret_ref"],
        "op://ai-agents/GITHUB_TOKEN/credential"
    );
    assert_eq!(
        cfg["transforms"][1]["config"]["secrets"][0]["source"]["secret_ref"],
        "op://ai-agents/OPENAI_API_KEY/credential"
    );
    let token = &cfg["transforms"][2]["config"]["tokens"][0];
    assert_eq!(
        token["client_id"]["secret_ref"],
        "op://ai-agents/GOOGLE_OAUTH_JSON/credential"
    );
    assert_eq!(token["client_id"]["json_key"], "client_id");
    assert_eq!(
        cfg["postgres"][0]["upstream"]["dsn"]["secret_ref"],
        "op://ai-agents/WAREHOUSE_DSN_UPSTREAM/credential"
    );
    assert!(cfg["postgres"][0]["sandbox_env"].is_null());
    assert!(!rendered.contains("placeholder:"));
}

#[test]
fn assigns_stable_secret_ids() {
    let fragment = fragment(
        r#"
transforms:
  - name: secrets
    config:
      secrets:
        - id: explicit-api-key
          replace: { proxy_value: OPENAI_API_KEY }
          rules: [{ host: api.openai.com }]
        - replace: { proxy_value: OPENAI_API_KEY }
          rules: [{ host: api.openai.com }]
        - source: { type: token_broker, credential_id: openai-codex }
          inject: { header: Authorization, formatter: "Bearer {{.Value}}" }
          rules: [{ host: chatgpt.com }]
"#,
    );

    let ids = |policy| {
        let (_, cfg) = render_cfg(std::slice::from_ref(&fragment), policy);
        cfg["transforms"][1]["config"]["secrets"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|secret| secret["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let env_ids = ids(SourcePolicy::env());
    let op_ids = ids(SourcePolicy::onepassword_connect("ai-agents", "10m"));

    assert_eq!(env_ids, op_ids);
    assert_eq!(env_ids[0], "explicit-api-key");
    assert!(env_ids[1].starts_with("openai-api-key-"));
    assert!(env_ids[2].starts_with("openai-codex-"));
}

#[test]
fn extracts_listen_ports() {
    let rendered = render_proxy_yaml(
        None,
        &[fragment(
            r#"
postgres:
  - name: warehouse
    listen: 0.0.0.0:5432
    upstream: { dsn: { type: env, var: WAREHOUSE_DSN } }
    client: { user: app_user, password_env: PG_PROXY_PASSWORD_WAREHOUSE }
"#,
        )],
    )
    .unwrap();
    let ports = listen_ports_from_yaml(&rendered).unwrap();
    assert_eq!(ports.all, vec![5432, 8080]);
    assert_eq!(ports.proxy, 8080);

    let rendered = render_proxy_yaml(Some("proxy:\n  tunnel_listen: ':18080'\n"), &[]).unwrap();
    let ports = listen_ports_from_yaml(&rendered).unwrap();
    assert_eq!(ports.all, vec![18080]);
    assert_eq!(ports.proxy, 18080);
}

#[test]
fn loads_builtin_fragments() {
    let dirs = default_harness_fragment_dirs();
    let discovered = discover_harness_fragment_files(&dirs).unwrap();
    assert_eq!(discovered.len(), 4);
    assert!(
        discovered
            .iter()
            .any(|file| { file.engine == "codex" && file.auth_mode == "api_key" })
    );
    assert!(
        discovered
            .iter()
            .any(|file| { file.engine == "claude-code" && file.auth_mode == "access_token" })
    );

    let codex = harness_fragment_from_dirs("codex", "api_key", &dirs)
        .unwrap()
        .unwrap();
    assert_eq!(
        placeholder_env(&[codex]),
        BTreeMap::from([("OPENAI_API_KEY".to_owned(), "OPENAI_API_KEY".to_owned())])
    );

    let codex_access = harness_fragment_from_dirs("codex", "access_token", &dirs)
        .unwrap()
        .unwrap();
    let (rendered, _) = render_cfg(
        &[codex_access],
        SourcePolicy::onepassword("ai-agents", "10m"),
    );
    assert!(rendered.contains("token_broker"));
    assert!(rendered.contains("chatgpt-account-id"));
    assert!(!rendered.contains("placeholder:"));

    let infra = infra_fragment().unwrap();
    let placeholders = placeholder_env(&[infra]);
    for name in ["AMP_API_KEY", "GITHUB_TOKEN", "SLACK_BOT_TOKEN"] {
        assert_eq!(placeholders.get(name).map(String::as_str), Some(name));
    }
}

#[test]
fn renders_token_broker_config() {
    let mut fragments = harness_broker_fragments().unwrap();
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

#[test]
fn discovers_tool_local_iron_yaml_fragments() {
    let root = temp_dir("discover");
    let base_tool = root.join("tools").join("base").join("websearch");
    let overlay_tool = root.join("overlay").join("tools").join("slack");
    fs::create_dir_all(&base_tool).unwrap();
    fs::create_dir_all(&overlay_tool).unwrap();
    fs::write(base_tool.join("iron.yaml"), "transforms: []\n").unwrap();
    fs::write(overlay_tool.join("iron.yaml"), "transforms: []\n").unwrap();
    fs::write(root.join("iron-proxy.yaml"), "transforms: []\n").unwrap();

    let discovered = discover_fragment_files(&[root.join("tools"), root.join("overlay")])
        .unwrap()
        .into_iter()
        .map(|path| path.strip_prefix(&root).unwrap().to_path_buf())
        .collect::<Vec<_>>();

    assert_eq!(
        discovered,
        vec![
            PathBuf::from("overlay/tools/slack/iron.yaml"),
            PathBuf::from("tools/base/websearch/iron.yaml"),
        ]
    );

    fs::remove_dir_all(root).unwrap();
}
