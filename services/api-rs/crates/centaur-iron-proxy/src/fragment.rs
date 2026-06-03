use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde_yaml::{Mapping, Value};
use toml::Value as TomlValue;

use crate::{
    IronProxyConfigError, ProxyFragment, Result, Secret, SecretReplace, Transform, TransformConfig,
};

const DEFAULT_INFRA_FRAGMENT_PATH: &str = "services/iron-proxy/infra.yaml";

pub fn load_fragment_file(path: impl AsRef<Path>) -> Result<ProxyFragment> {
    let path = path.as_ref();
    if path.file_name().and_then(|name| name.to_str()) == Some("pyproject.toml") {
        return load_pyproject_fragment_file(path);
    }
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

fn load_pyproject_fragment_file(path: &Path) -> Result<ProxyFragment> {
    let contents = read_file(path)?;
    let pyproject: TomlValue =
        toml::from_str(&contents).map_err(|source| IronProxyConfigError::ParsePyproject {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(pyproject_fragment(&pyproject))
}

fn pyproject_fragment(pyproject: &TomlValue) -> ProxyFragment {
    // TODO: Convert pyproject tool metadata into yaml fragments eventually so
    // this path uses the same fragment representation as iron.yaml.
    let Some(centaur) = pyproject
        .get("tool")
        .and_then(|tool| tool.get("centaur"))
        .and_then(TomlValue::as_table)
    else {
        return ProxyFragment::default();
    };

    let default_hosts = string_array(centaur.get("hosts"));
    let mut http_secrets = Vec::new();
    let mut oauth_tokens = Vec::new();

    for secret in centaur
        .get("secrets")
        .and_then(TomlValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(TomlValue::as_table)
    {
        match string_field(secret, "type") {
            Some("http") => {
                if let Some(parsed) = http_secret_from_tool_config(secret, &default_hosts) {
                    http_secrets.push(parsed);
                }
            }
            Some("oauth_token") => {
                if let Some(parsed) = oauth_token_from_tool_config(secret, &default_hosts) {
                    oauth_tokens.push(parsed);
                }
            }
            _ => {}
        }
    }

    let mut transforms = Vec::new();
    if !http_secrets.is_empty() {
        transforms.push(Transform {
            name: "secrets".to_owned(),
            config: TransformConfig {
                secrets: http_secrets,
                ..TransformConfig::default()
            },
            ..Transform::default()
        });
    }
    if !oauth_tokens.is_empty() {
        let mut extra = BTreeMap::new();
        extra.insert("tokens".to_owned(), Value::Sequence(oauth_tokens));
        transforms.push(Transform {
            name: "oauth_token".to_owned(),
            config: TransformConfig {
                extra,
                ..TransformConfig::default()
            },
            ..Transform::default()
        });
    }

    ProxyFragment {
        transforms,
        ..ProxyFragment::default()
    }
}

fn http_secret_from_tool_config(
    secret: &toml::value::Table,
    default_hosts: &[String],
) -> Option<Secret> {
    let proxy_value = string_field(secret, "name")?.to_owned();
    let hosts = hosts_for_secret(secret, default_hosts);
    if hosts.is_empty() {
        return None;
    }

    let mut replace_extra = BTreeMap::new();
    for key in ["match_headers", "match_path", "match_query"] {
        if let Some(value) = secret.get(key).and_then(toml_value_to_yaml) {
            replace_extra.insert(key.to_owned(), value);
        }
    }

    Some(Secret {
        replace: Some(SecretReplace {
            proxy_value: Some(proxy_value),
            extra: replace_extra,
        }),
        rules: host_rules(&hosts),
        ..Secret::default()
    })
}

fn oauth_token_from_tool_config(
    secret: &toml::value::Table,
    default_hosts: &[String],
) -> Option<Value> {
    let token_endpoint = string_field(secret, "token_endpoint")?;
    let hosts = hosts_for_secret(secret, default_hosts);
    if hosts.is_empty() {
        return None;
    }

    let mut token = Mapping::new();
    if let Some(grant) = string_field(secret, "grant") {
        token.insert(string_value("grant"), string_value(grant));
    }
    token.insert(string_value("token_endpoint"), string_value(token_endpoint));
    token.insert(string_value("rules"), Value::Sequence(host_rules(&hosts)));

    if let Some(fields) = secret.get("fields").and_then(TomlValue::as_table) {
        for (field_name, field_config) in fields {
            if let Some(source) = oauth_field_source(field_config) {
                token.insert(string_value(field_name), source);
            }
        }
    }

    Some(Value::Mapping(token))
}

fn oauth_field_source(field_config: &TomlValue) -> Option<Value> {
    if let Some(placeholder) = field_config.as_str().and_then(non_empty) {
        let mut source = Mapping::new();
        source.insert(string_value("placeholder"), string_value(placeholder));
        return Some(Value::Mapping(source));
    }

    let table = field_config.as_table()?;
    let placeholder =
        string_field(table, "placeholder").or_else(|| string_field(table, "secret_ref"))?;
    let mut source = Mapping::new();
    source.insert(string_value("placeholder"), string_value(placeholder));
    if let Some(json_key) = string_field(table, "json_key") {
        source.insert(string_value("json_key"), string_value(json_key));
    }
    Some(Value::Mapping(source))
}

fn hosts_for_secret(secret: &toml::value::Table, default_hosts: &[String]) -> Vec<String> {
    let hosts = string_array(secret.get("hosts"));
    if hosts.is_empty() {
        default_hosts.to_vec()
    } else {
        hosts
    }
}

fn host_rules(hosts: &[String]) -> Vec<Value> {
    hosts
        .iter()
        .map(|host| {
            let mut rule = Mapping::new();
            rule.insert(string_value("host"), string_value(host));
            Value::Mapping(rule)
        })
        .collect()
}

fn string_array(value: Option<&TomlValue>) -> Vec<String> {
    value
        .and_then(TomlValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(TomlValue::as_str)
        .filter_map(non_empty)
        .map(ToOwned::to_owned)
        .collect()
}

fn string_field<'a>(table: &'a toml::value::Table, key: &str) -> Option<&'a str> {
    table
        .get(key)
        .and_then(TomlValue::as_str)
        .and_then(non_empty)
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() { None } else { Some(value) }
}

fn toml_value_to_yaml(value: &TomlValue) -> Option<Value> {
    match value {
        TomlValue::String(value) => Some(string_value(value)),
        TomlValue::Integer(value) => serde_yaml::to_value(value).ok(),
        TomlValue::Float(value) => serde_yaml::to_value(value).ok(),
        TomlValue::Boolean(value) => Some(Value::Bool(*value)),
        TomlValue::Datetime(value) => Some(string_value(value.to_string())),
        TomlValue::Array(values) => Some(Value::Sequence(
            values.iter().filter_map(toml_value_to_yaml).collect(),
        )),
        TomlValue::Table(values) => {
            let values = values
                .iter()
                .filter_map(|(key, value)| Some((string_value(key), toml_value_to_yaml(value)?)))
                .collect();
            Some(Value::Mapping(values))
        }
    }
}

fn string_value(value: impl AsRef<str>) -> Value {
    Value::String(value.as_ref().to_owned())
}

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
