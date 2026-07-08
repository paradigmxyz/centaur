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
    // Bedrock authenticates with AWS SigV4 rather than a static bearer token:
    // codex's `amazon-bedrock` provider signs each request with the placeholder
    // AWS credentials below, and iron-proxy re-signs it with the real IAM keys
    // (same placeholder-swap model as the bearer providers, just for SigV4 —
    // see the `cloudwatch` tool). The endpoint host and signing region are
    // deploy-specific, so the fragment is built from `CODEX_BEDROCK_REGION` at
    // call time instead of baked as a const.
    if engine == "amazon-bedrock" && normalize_auth_mode(auth_mode) == "api_key" {
        return bedrock_aws_auth_fragment().map(Some);
    }
    let yaml = match (engine, normalize_auth_mode(auth_mode).as_str()) {
        ("codex", "api_key") => CODEX_API_KEY_FRAGMENT,
        ("codex", "access_token") => CODEX_ACCESS_TOKEN_FRAGMENT,
        ("openrouter", "api_key") => OPENROUTER_API_KEY_FRAGMENT,
        ("meta-ai", "api_key") => META_AI_API_KEY_FRAGMENT,
        ("claude-code", "api_key") => CLAUDE_CODE_API_KEY_FRAGMENT,
        ("claude-code", "access_token") => CLAUDE_CODE_ACCESS_TOKEN_FRAGMENT,
        _ => return Ok(None),
    };
    load_fragment_str(yaml).map(Some)
}

/// The deployment's Bedrock region. iron-proxy re-signs Bedrock requests for
/// this region only, and codex's `amazon-bedrock` provider talks to the
/// region-specific `bedrock-mantle.<region>.api.aws` endpoint, so both the
/// signing scope and the host rule derive from it. Defaults to `us-east-1`,
/// which is also codex's built-in `amazon-bedrock` base_url region.
pub fn bedrock_region() -> String {
    std::env::var("CODEX_BEDROCK_REGION")
        .ok()
        .map(|region| region.trim().to_owned())
        .filter(|region| !region.is_empty())
        .unwrap_or_else(|| "us-east-1".to_owned())
}

/// Whether the Bedrock provider is enabled for this deployment. Opt-in (it is
/// not the default codex provider): registering the `aws_auth` fragment without
/// AWS keys in the secrets backend would fail to resolve, so it only registers
/// when the operator sets `CODEX_BEDROCK_REGION`.
pub fn bedrock_enabled() -> bool {
    std::env::var("CODEX_BEDROCK_REGION")
        .ok()
        .is_some_and(|region| !region.trim().is_empty())
}

/// Whether the deployment's Bedrock IAM credentials are temporary (STS) and so
/// carry a session token. Long-term IAM user keys have none, and a session-token
/// placeholder with no backing secret would fail to resolve, so it is opt-in via
/// `CODEX_BEDROCK_SESSION_TOKEN`.
fn bedrock_uses_session_token() -> bool {
    std::env::var("CODEX_BEDROCK_SESSION_TOKEN")
        .ok()
        .is_some_and(|value| {
            let value = value.trim();
            !value.is_empty() && !value.eq_ignore_ascii_case("false") && value != "0"
        })
}

/// The sandbox env codex needs to sign Bedrock requests with *placeholder* AWS
/// credentials that iron-proxy then re-signs with the real IAM keys. The
/// access-key/secret/session-token values are placeholders the `aws_auth`
/// transform swaps; `AWS_REGION` is the real region (not a secret) so the client
/// signs for the same region iron-proxy is scoped to. `CODEX_BEDROCK_REGION` is
/// passed through so the sandbox entrypoint can pin codex's `amazon-bedrock`
/// provider to the same region (one source of truth — see entrypoint.sh). Empty
/// when Bedrock is disabled. The session-token placeholder is included only when
/// the fragment declares it, so the two never disagree (a bogus
/// `X-Amz-Security-Token` would break the signature). Mirrors the
/// `OPENAI_API_KEY`/`OPENROUTER_API_KEY` placeholder injection, but `aws_auth` is
/// not a `secrets` transform so [`placeholder_env`] does not cover it.
pub fn bedrock_sandbox_env() -> Vec<(String, String)> {
    if !bedrock_enabled() {
        return Vec::new();
    }
    bedrock_sandbox_env_for(&bedrock_region(), bedrock_uses_session_token())
}

/// Pure core of [`bedrock_sandbox_env`], split out so the placeholder set can be
/// asserted without setting process env in tests.
fn bedrock_sandbox_env_for(region: &str, uses_session_token: bool) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "AWS_ACCESS_KEY_ID".to_owned(),
            "AWS_ACCESS_KEY_ID".to_owned(),
        ),
        (
            "AWS_SECRET_ACCESS_KEY".to_owned(),
            "AWS_SECRET_ACCESS_KEY".to_owned(),
        ),
        ("AWS_REGION".to_owned(), region.to_owned()),
        ("CODEX_BEDROCK_REGION".to_owned(), region.to_owned()),
    ];
    if uses_session_token {
        env.push((
            "AWS_SESSION_TOKEN".to_owned(),
            "AWS_SESSION_TOKEN".to_owned(),
        ));
    }
    env
}

fn bedrock_aws_auth_fragment() -> Result<ProxyFragment> {
    load_fragment_str(&bedrock_aws_auth_fragment_yaml(
        &bedrock_region(),
        bedrock_uses_session_token(),
    ))
}

/// Pure core of [`bedrock_aws_auth_fragment`]. Keeping the YAML construction
/// env-free lets tests assert the host/region/scope without touching process
/// env.
fn bedrock_aws_auth_fragment_yaml(region: &str, uses_session_token: bool) -> String {
    // Temporary (STS) credentials carry a session token; long-term IAM user keys
    // do not. Only declare the placeholder when the operator opts in, so the
    // common long-term-key path needs no `AWS_SESSION_TOKEN` secret.
    let session_token_line = if uses_session_token {
        "      session_token: { placeholder: AWS_SESSION_TOKEN }\n"
    } else {
        ""
    };
    // codex's amazon-bedrock provider signs requests to the mantle endpoint with
    // SigV4 service `bedrock-mantle` (not the classic `bedrock`). iron-proxy's
    // aws_auth transform rejects (403 service_not_allowed, before reaching AWS)
    // any scope.service not in this list, so both forms are allowed.
    format!(
        r#"
transforms:
  - name: aws_auth
    config:
      access_key_id: {{ placeholder: AWS_ACCESS_KEY_ID }}
      secret_access_key: {{ placeholder: AWS_SECRET_ACCESS_KEY }}
{session_token_line}      allowed_services: [bedrock, bedrock-mantle]
      allowed_regions: [{region}]
      rules:
        - {{ host: bedrock-mantle.{region}.api.aws }}
"#
    )
}

const CODEX_API_KEY_FRAGMENT: &str = r#"
transforms:
  - name: secrets
    config:
      secrets:
        - id: OPENAI_API_KEY_AUTHORIZATION
          replace:
            proxy_value: OPENAI_API_KEY
            match_headers: ["Authorization"]
          rules: [{ host: api.openai.com }]
"#;

const OPENROUTER_API_KEY_FRAGMENT: &str = r#"
transforms:
  - name: secrets
    config:
      secrets:
        - id: OPENROUTER_API_KEY_AUTHORIZATION
          replace:
            proxy_value: OPENROUTER_API_KEY
            match_headers: ["Authorization"]
          rules: [{ host: openrouter.ai }]
"#;

const META_AI_API_KEY_FRAGMENT: &str = r#"
transforms:
  - name: secrets
    config:
      secrets:
        - id: META_AI_API_KEY_AUTHORIZATION
          replace:
            proxy_value: META_AI_API_KEY
            match_headers: ["Authorization"]
          rules: [{ host: api.ai.meta.com }]
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
/// read credentials straight from the environment (for example codex's
/// `OPENAI_API_KEY`) rather than through the tool SDK, whose `StubBackend`
/// already hands back the key name. Only the infra/harness fragments have such
/// consumers; tool fragments are excluded at the call site.
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

#[cfg(test)]
mod bedrock_tests {
    use super::*;

    #[test]
    fn bedrock_fragment_long_term_keys_scopes_region_and_omits_session_token() {
        let yaml = bedrock_aws_auth_fragment_yaml("us-east-1", false);
        assert!(yaml.contains("bedrock-mantle.us-east-1.api.aws"));
        assert!(yaml.contains("allowed_services: [bedrock, bedrock-mantle]"));
        assert!(yaml.contains("allowed_regions: [us-east-1]"));
        assert!(!yaml.contains("session_token"));

        let fragment = load_fragment_str(&yaml).expect("bedrock fragment parses");
        assert_eq!(fragment.transforms.len(), 1);
        assert_eq!(fragment.transforms[0].name, "aws_auth");
    }

    #[test]
    fn bedrock_fragment_temporary_keys_declare_session_token_and_region() {
        let yaml = bedrock_aws_auth_fragment_yaml("eu-central-1", true);
        assert!(yaml.contains("bedrock-mantle.eu-central-1.api.aws"));
        assert!(yaml.contains("allowed_regions: [eu-central-1]"));
        assert!(yaml.contains("session_token: { placeholder: AWS_SESSION_TOKEN }"));

        let fragment = load_fragment_str(&yaml).expect("bedrock fragment parses");
        assert_eq!(fragment.transforms[0].name, "aws_auth");
    }

    #[test]
    fn bedrock_sandbox_env_injects_placeholders_and_real_region() {
        let env = bedrock_sandbox_env_for("us-east-1", false);
        assert!(env.contains(&(
            "AWS_ACCESS_KEY_ID".to_owned(),
            "AWS_ACCESS_KEY_ID".to_owned()
        )));
        assert!(env.contains(&(
            "AWS_SECRET_ACCESS_KEY".to_owned(),
            "AWS_SECRET_ACCESS_KEY".to_owned()
        )));
        assert!(env.contains(&("AWS_REGION".to_owned(), "us-east-1".to_owned())));
        // Passed through so the sandbox entrypoint can pin codex's provider region.
        assert!(env.contains(&("CODEX_BEDROCK_REGION".to_owned(), "us-east-1".to_owned())));
        assert!(!env.iter().any(|(name, _)| name == "AWS_SESSION_TOKEN"));
    }

    #[test]
    fn bedrock_sandbox_env_adds_session_token_placeholder_when_temporary() {
        let env = bedrock_sandbox_env_for("eu-central-1", true);
        assert!(env.contains(&("AWS_REGION".to_owned(), "eu-central-1".to_owned())));
        assert!(env.contains(&(
            "AWS_SESSION_TOKEN".to_owned(),
            "AWS_SESSION_TOKEN".to_owned()
        )));
    }
}
