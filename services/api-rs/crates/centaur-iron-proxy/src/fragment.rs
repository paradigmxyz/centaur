use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::{IronProxyConfigError, ProxyFragment, Result};

const DEFAULT_INFRA_FRAGMENT_PATH: &str = "services/iron-proxy/infra.yaml";

pub fn load_fragment_file(path: impl AsRef<Path>) -> Result<ProxyFragment> {
    let path = path.as_ref();
    let contents = read_file(path)?;
    serde_yaml::from_str(&contents).map_err(|source| IronProxyConfigError::ParseFragment {
        path: path.to_path_buf(),
        source,
    })
}

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
        - replace:
            proxy_value: OPENAI_API_KEY
            match_headers: ["Authorization"]
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
    load_fragment_file(repo_relative_path(DEFAULT_INFRA_FRAGMENT_PATH))
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

fn repo_relative_path(relative: impl AsRef<Path>) -> PathBuf {
    let relative = relative.as_ref();
    let Ok(mut dir) = std::env::current_dir() else {
        return relative.to_path_buf();
    };
    loop {
        let candidate = dir.join(relative);
        if candidate.exists() {
            return candidate;
        }
        if !dir.pop() {
            return relative.to_path_buf();
        }
    }
}

fn read_file(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    fs::read_to_string(path).map_err(|source| IronProxyConfigError::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}
