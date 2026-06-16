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

/// The harness auth fragment for ``engine`` and ``auth_mode``. These are infra
/// — known in advance — so they are baked in rather than discovered from disk.
/// Returns ``None`` for an unknown engine/mode pair.
pub fn harness_auth_fragment(engine: &str, auth_mode: &str) -> Result<Option<ProxyFragment>> {
    let yaml = match (engine, normalize_auth_mode(auth_mode).as_str()) {
        ("codex", "api_key") => CODEX_API_KEY_FRAGMENT,
        ("codex", "access_token") => CODEX_ACCESS_TOKEN_FRAGMENT,
        ("openrouter", "api_key") => OPENROUTER_API_KEY_FRAGMENT,
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

const OPENROUTER_API_KEY_FRAGMENT: &str = r#"
transforms:
  - name: secrets
    config:
      secrets:
        - id: OPENROUTER_API_KEY_AUTHORIZATION
          source:
            placeholder: OPENROUTER_API_KEY
          inject:
            header: Authorization
            formatter: "Bearer {{.Value}}"
          rules: [{ host: openrouter.ai }]
"#;

// The `openai-codex` broker credential this references is managed by
// iron-control and provisioned out of band (see `centaur-perms broker create`).
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

// The `anthropic-claude` broker credential this references is managed by
// iron-control and provisioned out of band (see `centaur-perms broker create`).
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
"#;

pub fn infra_fragment() -> Result<ProxyFragment> {
    load_fragment_str(INFRA_FRAGMENT)
}

fn normalize_auth_mode(value: &str) -> String {
    value.replace('-', "_")
}

/// The `PLACEHOLDER=PLACEHOLDER` env for replace-mode secrets whose consumers
/// read credentials straight from the environment (codex's `OPENAI_API_KEY`,
/// git's `GITHUB_TOKEN`, …) rather than through the tool SDK, whose
/// `StubBackend` already hands back the key name. Only the infra/harness
/// fragments have such consumers; tool fragments are excluded at the call site.
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

/// The static catalog of sandbox Postgres DSN env vars declared across
/// ``fragments``: ``(env_var_name, database)``. The companion of
/// [`placeholder_env`] for `pg_dsn` secrets — each tool declares the DSN env
/// var `name` and `database` verbatim in its `pyproject.toml`, so the shape is
/// fixed at startup (tools don't hot-reload). iron-proxy multiplexes every
/// upstream through one listener (routing by database), so the DSNs differ only
/// by database; api-rs stamps the shared per-sandbox host/credential at create.
/// This lets every sandbox (warm/bootstrap included) be born with the full DSN
/// set without resolving a principal — the reassignable proxy enforces
/// per-principal access at runtime. Sorted and deduped for stable env ordering.
pub fn pg_sandbox_dsns(fragments: &[ProxyFragment]) -> Vec<(String, String)> {
    let mut dsns: Vec<(String, String)> = fragments
        .iter()
        .flat_map(|fragment| &fragment.postgres)
        .filter_map(|listener| {
            let sandbox_env = listener.sandbox_env.as_ref()?;
            Some((sandbox_env.name.clone()?, sandbox_env.database.clone()?))
        })
        .collect();
    dsns.sort();
    dsns.dedup();
    dsns
}
