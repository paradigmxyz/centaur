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

const DEFAULT_PROXY_BASE_CONFIG_PATH: &str = "services/api/api/iron-proxy.base.yaml";
const DEFAULT_INFRA_FRAGMENT_PATH: &str = "services/iron-proxy/infra.yaml";
const DEFAULT_HARNESS_FRAGMENT_DIR: &str = "services/iron-proxy/harness";
const API_KEY_FRAGMENT_SUFFIX: &str = "-api-key";
const ACCESS_TOKEN_FRAGMENT_SUFFIX: &str = "-access-token";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessFragmentFile {
    pub engine: String,
    pub auth_mode: String,
    pub path: PathBuf,
}

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

pub fn load_fragment_files(paths: &[PathBuf]) -> Result<Vec<ProxyFragment>> {
    paths.iter().map(load_fragment_file).collect()
}

pub fn discover_fragment_files(dirs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for dir in dirs {
        visit_fragment_dir(dir, &mut paths)?;
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub fn load_default_proxy_base_config() -> Result<String> {
    read_file(default_proxy_base_config_path())
}

pub fn default_harness_fragment_dirs() -> Vec<PathBuf> {
    vec![repo_relative_path(DEFAULT_HARNESS_FRAGMENT_DIR)]
}

pub fn discover_harness_fragment_files(dirs: &[PathBuf]) -> Result<Vec<HarnessFragmentFile>> {
    let mut files = Vec::new();
    for dir in dirs {
        visit_harness_fragment_dir(dir, &mut files)?;
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files.dedup_by(|left, right| left.path == right.path);
    Ok(files)
}

pub fn harness_fragment_from_dirs(
    engine: &str,
    auth_mode: &str,
    dirs: &[PathBuf],
) -> Result<Option<ProxyFragment>> {
    let auth_mode = normalize_auth_mode(auth_mode);
    let Some(fragment_file) = discover_harness_fragment_files(dirs)?
        .into_iter()
        .find(|file| file.engine == engine && file.auth_mode == auth_mode)
    else {
        return Ok(None);
    };
    load_fragment_file(fragment_file.path).map(Some)
}

pub fn harness_broker_fragments_from_dirs(dirs: &[PathBuf]) -> Result<Vec<ProxyFragment>> {
    discover_harness_fragment_files(dirs)?
        .into_iter()
        .filter(|file| file.auth_mode == "access_token")
        .map(|file| load_fragment_file(file.path))
        .collect()
}

fn visit_fragment_dir(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(dir).map_err(|source| IronProxyConfigError::ReadDir {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| IronProxyConfigError::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| IronProxyConfigError::ReadDir {
                path: path.clone(),
                source,
            })?;
        if file_type.is_dir() {
            visit_fragment_dir(&path, paths)?;
        } else if file_type.is_file() {
            match path.file_name().and_then(|name| name.to_str()) {
                Some("iron.yaml" | "pyproject.toml") => paths.push(path),
                _ => {}
            }
        }
    }
    Ok(())
}

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

fn visit_harness_fragment_dir(dir: &Path, files: &mut Vec<HarnessFragmentFile>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let entries = fs::read_dir(dir).map_err(|source| IronProxyConfigError::ReadDir {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| IronProxyConfigError::ReadDir {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| IronProxyConfigError::ReadDir {
                path: path.clone(),
                source,
            })?;
        if file_type.is_dir() {
            visit_harness_fragment_dir(&path, files)?;
        } else if file_type.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("yaml")
            && let Some(file) = parse_harness_fragment_file(path)
        {
            files.push(file);
        }
    }
    Ok(())
}

pub fn infra_fragment() -> Result<ProxyFragment> {
    load_fragment_file(repo_relative_path(DEFAULT_INFRA_FRAGMENT_PATH))
}

pub fn harness_broker_fragments() -> Result<Vec<ProxyFragment>> {
    harness_broker_fragments_from_dirs(&default_harness_fragment_dirs())
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

fn parse_harness_fragment_file(path: PathBuf) -> Option<HarnessFragmentFile> {
    let stem = path.file_stem()?.to_str()?;
    let (engine, auth_mode) = strip_auth_suffix(stem, API_KEY_FRAGMENT_SUFFIX, "api_key")
        .or_else(|| strip_auth_suffix(stem, ACCESS_TOKEN_FRAGMENT_SUFFIX, "access_token"))?;
    Some(HarnessFragmentFile {
        engine: engine.to_owned(),
        auth_mode: auth_mode.to_owned(),
        path,
    })
}

fn strip_auth_suffix<'a>(
    stem: &'a str,
    suffix: &str,
    auth_mode: &'static str,
) -> Option<(&'a str, &'static str)> {
    stem.strip_suffix(suffix)
        .filter(|engine| !engine.is_empty())
        .map(|engine| (engine, auth_mode))
}

fn normalize_auth_mode(value: &str) -> String {
    value.replace('-', "_")
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

fn default_proxy_base_config_path() -> PathBuf {
    repo_relative_path(DEFAULT_PROXY_BASE_CONFIG_PATH)
}

fn read_file(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    fs::read_to_string(path).map_err(|source| IronProxyConfigError::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}
