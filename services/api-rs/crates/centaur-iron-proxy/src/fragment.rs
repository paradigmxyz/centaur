use std::{collections::BTreeMap, path::PathBuf};

use crate::{IronProxyConfigError, ProxyFragment, Result};

/// The shared infra secrets, embedded at compile time so the binary carries no
/// runtime config-file dependency. The source lives in this crate so it's
/// always in the build context.
const INFRA_FRAGMENT: &str = include_str!("infra.yaml");

pub fn load_fragment_str(contents: &str) -> Result<ProxyFragment> {
    serde_yaml::from_str(contents).map_err(|source| IronProxyConfigError::ParseFragment {
        path: PathBuf::from("<inline>"),
        source,
    })
}

/// The harness auth fragment for ``engine`` (`codex`/`claude-code`) and
/// ``auth_mode`` (`api_key`/`access_token`). These are infra — known in advance
/// — so they are baked in rather than discovered from disk. Returns ``None``
/// for an unknown engine/mode pair.
pub fn harness_auth_fragment(engine: &str, auth_mode: &str) -> Result<Option<ProxyFragment>> {
    let yaml = match (engine, normalize_auth_mode(auth_mode).as_str()) {
        ("codex", "api_key") => CODEX_API_KEY_FRAGMENT,
        ("codex", "access_token") => CODEX_ACCESS_TOKEN_FRAGMENT,
        ("claude-code", "api_key") => CLAUDE_CODE_API_KEY_FRAGMENT,
        ("claude-code", "access_token") => CLAUDE_CODE_ACCESS_TOKEN_FRAGMENT,
        _ => return Ok(None),
    };
    load_fragment_str(yaml).map(Some)
}

const CODEX_API_KEY_FRAGMENT: &str = r#"
transforms:
  - name: secrets
    config:
      secrets:
        - id: OPENAI_API_KEY_AUTHORIZATION
          source:
            placeholder: OPENAI_API_KEY
          inject:
            header: Authorization
            formatter: "Bearer {{.Value}}"
          rules: [{ host: api.openai.com }]
"#;

const CODEX_ACCESS_TOKEN_FRAGMENT: &str = r#"
transforms:
  - name: secrets
    config:
      secrets:
        - source:
            type: token_broker
            credential_id: openai-codex
          inject:
            header: Authorization
            formatter: "Bearer {{.Value}}"
          rules: [{ host: chatgpt.com }]
        - source:
            placeholder: OPENAI_CODEX_ACCOUNT_ID
          inject:
            header: chatgpt-account-id
          rules: [{ host: chatgpt.com }]
broker_credentials:
  - id: openai-codex
    token_endpoint: https://auth.openai.com/oauth/token
    client_id:
      placeholder: OPENAI_CODEX_CLIENT_ID
    store:
      placeholder: OPENAI_CODEX_BLOB
"#;

const CLAUDE_CODE_API_KEY_FRAGMENT: &str = r#"
transforms:
  - name: secrets
    config:
      secrets:
        - replace:
            proxy_value: ANTHROPIC_API_KEY
            match_headers: ["X-Api-Key"]
          rules: [{ host: api.anthropic.com }]
"#;

const CLAUDE_CODE_ACCESS_TOKEN_FRAGMENT: &str = r#"
transforms:
  - name: secrets
    config:
      secrets:
        - source:
            type: token_broker
            credential_id: anthropic-claude
          inject:
            header: Authorization
            formatter: "Bearer {{.Value}}"
          rules: [{ host: api.anthropic.com }]
broker_credentials:
  - id: anthropic-claude
    token_endpoint: https://console.anthropic.com/v1/oauth/token
    client_id:
      placeholder: CLAUDE_CODE_CLIENT_ID
    store:
      placeholder: CLAUDE_CODE_BLOB
"#;

pub fn infra_fragment() -> Result<ProxyFragment> {
    load_fragment_str(INFRA_FRAGMENT)
}

fn normalize_auth_mode(value: &str) -> String {
    value.replace('-', "_")
}

pub fn placeholder_env(fragments: &[ProxyFragment]) -> BTreeMap<String, String> {
    fragments
        .iter()
        .flat_map(|fragment| &fragment.transforms)
        .filter(|transform| transform.is_secrets())
        .flat_map(|transform| &transform.config.secrets)
        .filter_map(|secret| secret.proxy_value())
        .filter(|value| !value.is_empty() && !value.contains('='))
        .map(|value| (value.to_owned(), value.to_owned()))
        .collect()
}
