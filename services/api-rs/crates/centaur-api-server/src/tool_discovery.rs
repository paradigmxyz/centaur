use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

use centaur_iron_control::{GCP_ID_TOKEN_ALLOWED_HEADERS, normalize_gcp_id_token_header};
use centaur_iron_proxy::{
    PgDsnSetting, PgDsnSettingValueFrom, PostgresListener, PostgresUpstream, ProxyFragment,
    SandboxEnv, Secret, SecretReplace, Transform, TransformConfig,
};
use centaur_session_runtime::{PersonaDefinition, PersonaRegistry};
use serde::Serialize;
use serde_yaml::Value as YamlValue;
use sha2::{Digest, Sha256};
use thiserror::Error;
use toml::Value as TomlValue;
use tracing::{info, warn};

const DEFAULT_MATCH_HEADERS: &[&str] = &[
    "Authorization",
    "Proxy-Authorization",
    "Api-Key",
    "Anthropic-Api-Key",
    "Auth-Token",
    "Jwt",
    "Cookie",
    "Apikey",
    "AccessKey",
    "Api-Access-Key",
    "Api-Signature",
    "FX-ACCESS-KEY",
    "FX-ACCESS-SIGN",
    "FX-ACCESS-PASSPHRASE",
    "X-CB-ACCESS-PASSPHRASE",
    "X-CB-ACCESS-SIGNATURE",
    "/^x-[a-z0-9-]*(api-key|apikey|secret|token|auth|key)$/",
];

#[derive(Clone, Debug, Default)]
pub struct ToolDiscoveryConfig {
    pub tool_dirs: Option<String>,
    pub public_tool_dirs: Option<String>,
    pub tools_path: Option<PathBuf>,
    pub tools_overlay_path: Option<PathBuf>,
    pub plugins_dir: Option<PathBuf>,
    pub tools_config: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct DiscoveredToolProxyFragment {
    pub fragment: ProxyFragment,
    pub tool_count: usize,
    pub secret_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoveredToolCatalog {
    pub(crate) tools: Vec<DiscoveredTool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiscoveredTool {
    pub(crate) name: String,
    pub(crate) package: String,
    pub(crate) description: Option<String>,
    pub(crate) client_module: String,
    pub(crate) project_dir: PathBuf,
}

#[derive(Debug, Error)]
pub enum ToolDiscoveryError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse TOML {path}: {source}")]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("failed to serialize tool proxy fragment: {0}")]
    Serialize(#[from] serde_yaml::Error),
    #[error("{0}")]
    Invalid(String),
}

impl ToolDiscoveryConfig {
    pub fn resolve_tool_dirs(&self) -> Result<Vec<PathBuf>, ToolDiscoveryError> {
        if let Some(tool_dirs) = clean_optional_str(self.tool_dirs.as_deref()) {
            return Ok(split_tool_dirs(&tool_dirs));
        }

        let mut sandbox_style_dirs = Vec::new();
        if let Some(path) = clean_optional_path(self.tools_path.as_deref()) {
            sandbox_style_dirs.push(path);
        }
        if let Some(path) = clean_optional_path(self.tools_overlay_path.as_deref()) {
            sandbox_style_dirs.push(path);
        }
        if !sandbox_style_dirs.is_empty() {
            return Ok(sandbox_style_dirs);
        }

        if let Some(config_path) = self
            .tools_config
            .as_deref()
            .map(Path::to_path_buf)
            .or_else(default_tools_config)
        {
            let dirs = load_plugins_config(&config_path)?;
            if !dirs.is_empty() {
                return Ok(dirs);
            }
        }

        if let Some(path) = clean_optional_path(self.plugins_dir.as_deref()) {
            return Ok(vec![path]);
        }

        let root = default_repo_root().unwrap_or_else(|| {
            env::current_dir()
                .ok()
                .unwrap_or_else(|| PathBuf::from("."))
        });
        Ok(vec![root.join("tools")])
    }

    pub fn resolve_public_tool_dirs(&self) -> Vec<PathBuf> {
        self.public_tool_dirs
            .as_deref()
            .and_then(|value| clean_optional_str(Some(value)))
            .map(|value| split_tool_dirs(&value))
            .unwrap_or_default()
    }
}

pub fn discover_tool_proxy_fragment(
    tool_dirs: &[PathBuf],
) -> Result<DiscoveredToolProxyFragment, ToolDiscoveryError> {
    let tools = collect_plugin_metadata(tool_dirs)?.tools;
    let mut secrets = Vec::new();
    for tool in &tools {
        secrets.extend(tool.secrets.iter().cloned());
    }
    let secret_count = secrets.len();
    let fragment = fragment_from_secrets(secrets)?;
    info!(
        tool_dirs = ?tool_dirs,
        tool_count = tools.len(),
        secret_count,
        "discovered api-rs tool proxy secrets"
    );
    Ok(DiscoveredToolProxyFragment {
        fragment,
        tool_count: tools.len(),
        secret_count,
    })
}

pub fn discover_persona_registry(
    tool_dirs: &[PathBuf],
    default_persona_id: Option<String>,
) -> Result<PersonaRegistry, ToolDiscoveryError> {
    let plugins = collect_plugin_metadata(tool_dirs)?;
    PersonaRegistry::new(plugins.personas, default_persona_id, plugins.overlay_chain)
        .map_err(ToolDiscoveryError::Invalid)
}

pub(crate) fn discover_tool_catalog(
    tool_dirs: &[PathBuf],
) -> Result<DiscoveredToolCatalog, ToolDiscoveryError> {
    let mut tools = Vec::new();
    for tool in collect_plugin_metadata(tool_dirs)?.tools {
        for script_name in tool.script_names {
            tools.push(DiscoveredTool {
                name: script_name,
                package: tool.package.clone(),
                description: tool.description.clone(),
                client_module: tool.client_module.clone(),
                project_dir: tool.dir.clone(),
            });
        }
    }
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(DiscoveredToolCatalog { tools })
}

fn split_tool_dirs(value: &str) -> Vec<PathBuf> {
    value
        .split(':')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn clean_optional_str(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn clean_optional_path(value: Option<&Path>) -> Option<PathBuf> {
    value.and_then(|path| {
        let value = path.to_string_lossy();
        (!value.trim().is_empty()).then(|| path.to_path_buf())
    })
}

fn tool_labels(tool: &str, overlay: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("centaur-tool".to_owned(), tool.to_owned()),
        ("centaur-tool-overlay".to_owned(), overlay.to_owned()),
    ])
}

fn overlay_name_for_root(root: &Path) -> String {
    let candidate = if root.file_name().and_then(|name| name.to_str()) == Some("tools") {
        root.parent().and_then(|parent| parent.file_name())
    } else {
        root.file_name()
    };
    candidate
        .and_then(|name| name.to_str())
        .map(centaur_iron_control::slugify)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn merge_tool_labels(target: &mut BTreeMap<String, String>, incoming: &BTreeMap<String, String>) {
    merge_csv_label(target, incoming, "centaur-tool");
    merge_csv_label(target, incoming, "centaur-tool-overlay");
}

fn merge_csv_label(
    target: &mut BTreeMap<String, String>,
    incoming: &BTreeMap<String, String>,
    key: &str,
) {
    let mut values = BTreeSet::new();
    if let Some(existing) = target.get(key) {
        values.extend(
            existing
                .split(',')
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        );
    }
    if let Some(next) = incoming.get(key) {
        values.extend(
            next.split(',')
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        );
    }
    if !values.is_empty() {
        target.insert(
            key.to_owned(),
            values.into_iter().collect::<Vec<_>>().join(","),
        );
    }
}

fn default_tools_config() -> Option<PathBuf> {
    let cwd = env::current_dir().ok()?;
    find_ancestor_file(&cwd, "tools.toml")
}

fn default_repo_root() -> Option<PathBuf> {
    default_tools_config().and_then(|path| path.parent().map(Path::to_path_buf))
}

fn find_ancestor_file(start: &Path, name: &str) -> Option<PathBuf> {
    for dir in start.ancestors() {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn load_plugins_config(config_path: &Path) -> Result<Vec<PathBuf>, ToolDiscoveryError> {
    if !config_path.exists() {
        return Ok(Vec::new());
    }
    let contents = fs::read_to_string(config_path).map_err(|source| ToolDiscoveryError::Read {
        path: config_path.to_path_buf(),
        source,
    })?;
    let data = parse_toml(config_path, &contents)?;
    let base = config_path.parent().unwrap_or_else(|| Path::new("."));
    Ok(data
        .get("plugin_dirs")
        .and_then(TomlValue::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(TomlValue::as_str)
                .map(|entry| {
                    let path = PathBuf::from(entry);
                    if path.is_absolute() {
                        path
                    } else {
                        base.join(path)
                    }
                })
                .collect()
        })
        .unwrap_or_default())
}

fn parse_toml(path: &Path, contents: &str) -> Result<TomlValue, ToolDiscoveryError> {
    toml::from_str(contents).map_err(|source| ToolDiscoveryError::Toml {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Clone, Debug)]
struct LoadedToolMeta {
    name: String,
    dir: PathBuf,
    package: String,
    description: Option<String>,
    client_module: String,
    script_names: Vec<String>,
    secrets: Vec<ToolSecret>,
}

#[derive(Clone, Debug, Default)]
struct CollectedPluginMetadata {
    tools: Vec<LoadedToolMeta>,
    personas: Vec<PersonaDefinition>,
    overlay_chain: Vec<String>,
}

#[derive(Clone, Debug)]
enum LoadedPluginMeta {
    Tool(LoadedToolMeta),
    Persona(PersonaDefinition),
}

fn collect_plugin_metadata(
    tool_dirs: &[PathBuf],
) -> Result<CollectedPluginMetadata, ToolDiscoveryError> {
    let mut seen_tools = BTreeMap::<String, usize>::new();
    let mut seen_personas = BTreeMap::<String, usize>::new();
    let mut tools = Vec::<LoadedToolMeta>::new();
    let mut personas = Vec::<PersonaDefinition>::new();
    let mut overlay_chain = Vec::new();
    let mut existing = false;

    for (dir_idx, base_dir) in tool_dirs.iter().enumerate() {
        overlay_chain.push(base_dir.display().to_string());
        if !base_dir.exists() {
            continue;
        }
        existing = true;
        for tool_dir in candidate_tool_dirs(base_dir)? {
            let pyproject_path = tool_dir.join("pyproject.toml");
            if !pyproject_path.exists() {
                continue;
            }
            let Some(meta) = load_plugin_meta(base_dir, &tool_dir, &pyproject_path)? else {
                continue;
            };
            match meta {
                LoadedPluginMeta::Tool(meta) => {
                    if let Some(prev_dir_idx) = seen_tools.insert(meta.name.clone(), dir_idx) {
                        if let Some(prev_pos) = tools.iter().position(|tool| tool.name == meta.name)
                        {
                            warn!(
                                tool = %meta.name,
                                shadowed_dir = ?tool_dirs.get(prev_dir_idx),
                                by_dir = ?base_dir,
                                "api-rs tool metadata shadowed"
                            );
                            tools[prev_pos] = meta;
                        }
                    } else {
                        tools.push(meta);
                    }
                }
                LoadedPluginMeta::Persona(persona) => {
                    if let Some(prev_dir_idx) = seen_personas.insert(persona.id.clone(), dir_idx) {
                        if let Some(prev_pos) =
                            personas.iter().position(|item| item.id == persona.id)
                        {
                            warn!(
                                persona = %persona.id,
                                shadowed_dir = ?tool_dirs.get(prev_dir_idx),
                                by_dir = ?base_dir,
                                "api-rs persona metadata shadowed"
                            );
                            personas[prev_pos] = persona;
                        }
                    } else {
                        personas.push(persona);
                    }
                }
            }
        }
    }

    if !existing {
        info!(tool_dirs = ?tool_dirs, "api-rs tool dirs missing");
    }
    Ok(CollectedPluginMetadata {
        tools,
        personas,
        overlay_chain,
    })
}

fn candidate_tool_dirs(base_dir: &Path) -> Result<Vec<PathBuf>, ToolDiscoveryError> {
    let mut candidates = Vec::new();
    let mut children = read_dirs_sorted(base_dir)?;
    children.retain(|path| is_visible_dir(path));
    for child in children {
        if child.join("pyproject.toml").exists() {
            candidates.push(child);
            continue;
        }
        let mut grandchildren = read_dirs_sorted(&child)?;
        grandchildren.retain(|path| is_visible_dir(path));
        for grandchild in grandchildren {
            if grandchild.join("pyproject.toml").exists() {
                candidates.push(grandchild);
            }
        }
    }
    Ok(candidates)
}

fn read_dirs_sorted(path: &Path) -> Result<Vec<PathBuf>, ToolDiscoveryError> {
    let mut dirs = fs::read_dir(path)
        .map_err(|source| ToolDiscoveryError::Read {
            path: path.to_path_buf(),
            source,
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect::<Vec<_>>();
    dirs.sort();
    Ok(dirs)
}

fn is_visible_dir(path: &Path) -> bool {
    path.is_dir()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| !name.starts_with('.') && !name.starts_with('_'))
}

fn load_plugin_meta(
    source_root: &Path,
    plugin_dir: &Path,
    pyproject_path: &Path,
) -> Result<Option<LoadedPluginMeta>, ToolDiscoveryError> {
    let contents =
        fs::read_to_string(pyproject_path).map_err(|source| ToolDiscoveryError::Read {
            path: pyproject_path.to_path_buf(),
            source,
        })?;
    let pyproject = match parse_toml(pyproject_path, &contents) {
        Ok(pyproject) => pyproject,
        Err(error) => {
            warn!(
                plugin_dir = ?plugin_dir,
                error = %error,
                "api-rs plugin pyproject parse failed"
            );
            return Ok(None);
        }
    };
    let default_tool_conf = TomlValue::Table(Default::default());
    let tool_conf = pyproject
        .get("tool")
        .and_then(|value| value.get("centaur"))
        .unwrap_or(&default_tool_conf);
    if tool_conf.get("type").and_then(TomlValue::as_str) != Some("persona") {
        return load_tool_meta(source_root, plugin_dir, &pyproject, tool_conf)
            .map(|meta| meta.map(LoadedPluginMeta::Tool));
    }
    let id = plugin_dir
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            ToolDiscoveryError::Invalid(format!("invalid persona path {}", plugin_dir.display()))
        })?
        .to_owned();
    let prompt_file = tool_conf
        .get("prompt_file")
        .or_else(|| tool_conf.get("prompt"))
        .and_then(TomlValue::as_str)
        .unwrap_or("PROMPT.md");
    let prompt_path = plugin_dir.join(prompt_file);
    let prompt = fs::read_to_string(&prompt_path).map_err(|source| ToolDiscoveryError::Read {
        path: prompt_path.clone(),
        source,
    })?;
    let prompt_hash = {
        let digest = Sha256::digest(prompt.as_bytes());
        format!("sha256:{digest:x}")
    };
    Ok(Some(LoadedPluginMeta::Persona(PersonaDefinition {
        id,
        source_root: source_root.display().to_string(),
        source_path: plugin_dir.display().to_string(),
        source_ref: None,
        prompt_hash,
        prompt,
    })))
}

fn load_tool_meta(
    source_root: &Path,
    tool_dir: &Path,
    pyproject: &TomlValue,
    tool_conf: &TomlValue,
) -> Result<Option<LoadedToolMeta>, ToolDiscoveryError> {
    let name = tool_dir
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            ToolDiscoveryError::Invalid(format!("invalid tool path {}", tool_dir.display()))
        })?
        .to_owned();
    let default_project_conf = TomlValue::Table(Default::default());
    let project_conf = pyproject.get("project").unwrap_or(&default_project_conf);
    let package = project_conf
        .get("name")
        .and_then(TomlValue::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| name.clone());
    let description = project_conf
        .get("description")
        .and_then(TomlValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let client_module = tool_conf
        .get("module")
        .and_then(TomlValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("client.py")
        .to_owned();
    let script_names = project_conf
        .get("scripts")
        .and_then(TomlValue::as_table)
        .map(|scripts| {
            let mut names = scripts
                .keys()
                .filter(|script| !script.contains('/') && !script.contains('\0'))
                .cloned()
                .collect::<Vec<_>>();
            names.sort();
            names
        })
        .filter(|names| !names.is_empty())
        .unwrap_or_else(|| vec![name.clone()]);
    let default_hosts = string_array(tool_conf.get("hosts"));
    let labels = tool_labels(&name, &overlay_name_for_root(source_root));
    let secrets = match parse_secret_list(tool_conf.get("secrets"), &default_hosts, &labels)
        .and_then(|mut secrets| {
            secrets.extend(parse_secret_list(
                tool_conf.get("optional_secrets"),
                &default_hosts,
                &labels,
            )?);
            Ok(secrets)
        }) {
        Ok(secrets) => secrets,
        Err(error) => {
            warn!(
                tool = %name,
                error = %error,
                "api-rs tool invalid secret metadata"
            );
            return Ok(None);
        }
    };
    Ok(Some(LoadedToolMeta {
        name,
        dir: tool_dir.to_path_buf(),
        package,
        description,
        client_module,
        script_names,
        secrets,
    }))
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ToolSecret {
    Http(HttpSecret),
    OAuthToken(OAuthTokenSecret),
    GcpAuth(GcpAuthSecret),
    GcpIdToken(GcpIdTokenSecret),
    PgDsn(PgDsnSecret),
    AwsAuth(AwsAuthSecret),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HttpSecret {
    name: String,
    secret_ref: String,
    labels: BTreeMap<String, String>,
    mode: HttpSecretMode,
    hosts: Vec<String>,
    replacer: String,
    match_headers: Vec<String>,
    match_path: bool,
    match_query: bool,
    inject_header: String,
    inject_formatter: String,
    inject_query_param: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum HttpSecretMode {
    Replace,
    Inject,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct OAuthFieldSource {
    secret_ref: String,
    json_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct OAuthTokenSecret {
    name: String,
    labels: BTreeMap<String, String>,
    grant: String,
    hosts: Vec<String>,
    fields: Vec<(String, OAuthFieldSource)>,
    scopes: Vec<String>,
    token_endpoint: Option<String>,
    token_endpoint_headers: Vec<(String, OAuthFieldSource)>,
    audience: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GcpAuthSecret {
    name: String,
    secret_ref: String,
    labels: BTreeMap<String, String>,
    hosts: Vec<String>,
    scopes: Vec<String>,
}

/// A `type = "gcp_id_token"` secret. The proxy mints a Google-signed OIDC ID
/// token from the service-account keyfile and injects it into Authorization by
/// default, or into `x-serverless-authorization` when configured for Cloud Run.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GcpIdTokenSecret {
    secret_ref: String,
    labels: BTreeMap<String, String>,
    hosts: Vec<String>,
    audience: String,
    header: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PgDsnSecret {
    name: String,
    secret_ref: String,
    labels: BTreeMap<String, String>,
    database: String,
    role: Option<String>,
    settings: Vec<PgDsnSetting>,
}

/// A `type = "aws_auth"` secret. The in-sandbox AWS SDK signs each request with
/// throwaway placeholder credentials (`access_key_id_ref`/`secret_access_key_ref`
/// and the optional `session_token_ref`); iron-proxy's `aws_auth` transform reads
/// the region/service from the inbound signature scope and re-signs with the real
/// keys resolved from those refs. `allowed_regions`/`allowed_services` scope which
/// the proxy will sign for; `hosts` become the request rules. Mirrors the
/// `centaur-perms` parser so both producers agree on the metadata.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AwsAuthSecret {
    name: String,
    labels: BTreeMap<String, String>,
    hosts: Vec<String>,
    access_key_id_ref: String,
    secret_access_key_ref: String,
    session_token_ref: Option<String>,
    allowed_regions: Vec<String>,
    allowed_services: Vec<String>,
}

fn parse_secret_list(
    value: Option<&TomlValue>,
    default_hosts: &[String],
    labels: &BTreeMap<String, String>,
) -> Result<Vec<ToolSecret>, ToolDiscoveryError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = value
        .as_array()
        .ok_or_else(|| ToolDiscoveryError::Invalid("'secrets' must be an array".to_owned()))?;
    entries
        .iter()
        .map(|entry| parse_secret(entry, default_hosts, labels))
        .collect()
}

fn parse_secret(
    value: &TomlValue,
    default_hosts: &[String],
    labels: &BTreeMap<String, String>,
) -> Result<ToolSecret, ToolDiscoveryError> {
    if let Some(name) = value.as_str() {
        let name = nonempty(name, "secret name")?.to_owned();
        if default_hosts.is_empty() {
            return Err(ToolDiscoveryError::Invalid(format!(
                "secret entry {name:?} requires the tool to declare non-empty top-level \
                 'hosts'; a secret without hosts would be unscoped in iron-proxy"
            )));
        }
        return Ok(ToolSecret::Http(HttpSecret {
            name: name.clone(),
            secret_ref: name.clone(),
            labels: labels.clone(),
            mode: HttpSecretMode::Replace,
            hosts: default_hosts.to_vec(),
            replacer: name,
            match_headers: DEFAULT_MATCH_HEADERS
                .iter()
                .map(ToString::to_string)
                .collect(),
            match_path: false,
            match_query: false,
            inject_header: String::new(),
            inject_formatter: String::new(),
            inject_query_param: String::new(),
        }));
    }
    let table = value.as_table().ok_or_else(|| {
        ToolDiscoveryError::Invalid("secret entry must be a string or table".to_owned())
    })?;
    let name = required_str(table, "name")?.to_owned();
    let secret_ref = optional_str(table, "secret_ref")
        .unwrap_or(name.as_str())
        .to_owned();
    match optional_str(table, "type").unwrap_or("http") {
        "http" | "header" => parse_http_secret(table, name, secret_ref, default_hosts, labels),
        "oauth_token" => parse_oauth_token_secret(table, name, labels),
        "gcp_auth" => parse_gcp_auth_secret(table, name, secret_ref, labels),
        "gcp_id_token" => parse_gcp_id_token_secret(table, name, secret_ref, labels),
        "pg_dsn" => parse_pg_dsn_secret(table, name, secret_ref, labels),
        "aws_auth" => parse_aws_auth_secret(table, name, labels),
        "brokered_token" | "hmac_sign" => Err(ToolDiscoveryError::Invalid(format!(
            "api-rs iron-control tool discovery does not yet support secret type {:?}",
            optional_str(table, "type").unwrap_or("unknown")
        ))),
        other => Err(ToolDiscoveryError::Invalid(format!(
            "unknown secret type {other:?}"
        ))),
    }
}

fn parse_http_secret(
    table: &toml::Table,
    name: String,
    secret_ref: String,
    default_hosts: &[String],
    labels: &BTreeMap<String, String>,
) -> Result<ToolSecret, ToolDiscoveryError> {
    let hosts =
        optional_string_array(table.get("hosts"))?.unwrap_or_else(|| default_hosts.to_vec());
    if hosts.is_empty() || hosts.iter().any(String::is_empty) {
        return Err(ToolDiscoveryError::Invalid(format!(
            "HTTP secret {name:?} 'hosts' must be a non-empty array of non-empty strings \
             (entry-level or tool-level); a secret without hosts would be unscoped in \
             iron-proxy"
        )));
    }
    let mode = optional_str(table, "mode").unwrap_or("replace");
    match mode {
        "replace" => {
            let match_headers =
                optional_string_array(table.get("match_headers"))?.unwrap_or_default();
            let match_path = optional_bool(table, "match_path")?.unwrap_or(false);
            let match_query = optional_bool(table, "match_query")?.unwrap_or(false);
            if match_headers.is_empty() && !match_path && !match_query {
                return Err(ToolDiscoveryError::Invalid(format!(
                    "replace-mode HTTP secret {name:?} must declare match_headers, match_path, or match_query"
                )));
            }
            let replacer = optional_str(table, "replacer").unwrap_or(&name).to_owned();
            Ok(ToolSecret::Http(HttpSecret {
                name,
                secret_ref,
                labels: labels.clone(),
                mode: HttpSecretMode::Replace,
                hosts,
                replacer,
                match_headers,
                match_path,
                match_query,
                inject_header: String::new(),
                inject_formatter: String::new(),
                inject_query_param: String::new(),
            }))
        }
        "inject" => {
            let inject_header = optional_str(table, "inject_header")
                .unwrap_or_default()
                .to_owned();
            let inject_query_param = optional_str(table, "inject_query_param")
                .unwrap_or_default()
                .to_owned();
            let inject_formatter = optional_str(table, "inject_formatter")
                .unwrap_or_default()
                .to_owned();
            if inject_header.is_empty() == inject_query_param.is_empty() {
                return Err(ToolDiscoveryError::Invalid(format!(
                    "inject-mode HTTP secret {name:?} must declare exactly one of inject_header or inject_query_param"
                )));
            }
            if !inject_formatter.is_empty() && inject_header.is_empty() {
                return Err(ToolDiscoveryError::Invalid(format!(
                    "inject-mode HTTP secret {name:?} sets inject_formatter without inject_header"
                )));
            }
            Ok(ToolSecret::Http(HttpSecret {
                name,
                secret_ref,
                labels: labels.clone(),
                mode: HttpSecretMode::Inject,
                hosts,
                replacer: String::new(),
                match_headers: Vec::new(),
                match_path: false,
                match_query: false,
                inject_header,
                inject_formatter,
                inject_query_param,
            }))
        }
        other => Err(ToolDiscoveryError::Invalid(format!(
            "unknown HTTP secret mode {other:?}"
        ))),
    }
}

fn parse_oauth_token_secret(
    table: &toml::Table,
    name: String,
    labels: &BTreeMap<String, String>,
) -> Result<ToolSecret, ToolDiscoveryError> {
    let grant = required_str(table, "grant")?.to_owned();
    let hosts = required_string_array(table.get("hosts"), "hosts")?;
    let scopes = optional_string_array(table.get("scopes"))?.unwrap_or_default();
    let token_endpoint = optional_str(table, "token_endpoint").map(ToOwned::to_owned);
    let fields = parse_oauth_fields(table.get("fields"), &name)?;
    let token_endpoint_headers = parse_oauth_fields(table.get("token_endpoint_headers"), &name)?
        .into_iter()
        .collect();
    let audience = optional_str(table, "audience").map(ToOwned::to_owned);
    Ok(ToolSecret::OAuthToken(OAuthTokenSecret {
        name,
        labels: labels.clone(),
        grant,
        hosts,
        fields,
        scopes,
        token_endpoint,
        token_endpoint_headers,
        audience,
    }))
}

fn parse_gcp_auth_secret(
    table: &toml::Table,
    name: String,
    secret_ref: String,
    labels: &BTreeMap<String, String>,
) -> Result<ToolSecret, ToolDiscoveryError> {
    Ok(ToolSecret::GcpAuth(GcpAuthSecret {
        name,
        secret_ref,
        labels: labels.clone(),
        hosts: optional_string_array(table.get("hosts"))?.unwrap_or_default(),
        scopes: optional_string_array(table.get("scopes"))?.unwrap_or_default(),
    }))
}

fn parse_gcp_id_token_secret(
    table: &toml::Table,
    _name: String,
    secret_ref: String,
    labels: &BTreeMap<String, String>,
) -> Result<ToolSecret, ToolDiscoveryError> {
    let hosts = required_string_array(table.get("hosts"), "hosts")?;
    if hosts.is_empty() {
        return Err(ToolDiscoveryError::Invalid(
            "gcp_id_token hosts must be non-empty".to_owned(),
        ));
    }
    let audience = required_str(table, "audience")?.to_owned();
    let header = optional_str(table, "header")
        .map(ToOwned::to_owned)
        .map(validate_gcp_id_token_header)
        .transpose()?;
    Ok(ToolSecret::GcpIdToken(GcpIdTokenSecret {
        secret_ref,
        labels: labels.clone(),
        hosts,
        audience,
        header,
    }))
}

fn parse_pg_dsn_secret(
    table: &toml::Table,
    name: String,
    secret_ref: String,
    labels: &BTreeMap<String, String>,
) -> Result<ToolSecret, ToolDiscoveryError> {
    let database = required_str(table, "database")?.to_owned();
    let role = optional_str(table, "role").map(ToOwned::to_owned);
    let settings = parse_pg_dsn_settings(table.get("settings"))?;
    Ok(ToolSecret::PgDsn(PgDsnSecret {
        name,
        secret_ref,
        labels: labels.clone(),
        database,
        role,
        settings,
    }))
}

fn parse_pg_dsn_settings(
    value: Option<&TomlValue>,
) -> Result<Vec<PgDsnSetting>, ToolDiscoveryError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value.as_array().ok_or_else(|| {
        ToolDiscoveryError::Invalid("pg_dsn settings must be an array".to_owned())
    })?;
    array
        .iter()
        .map(|item| {
            let table = item.as_table().ok_or_else(|| {
                ToolDiscoveryError::Invalid("pg_dsn setting must be a table".to_owned())
            })?;
            let name = required_str(table, "name")?.to_owned();
            let literal = optional_str(table, "value").map(ToOwned::to_owned);
            let value_from = parse_pg_dsn_setting_value_from(table.get("value_from"))?;
            if literal.is_some() == value_from.is_some() {
                return Err(ToolDiscoveryError::Invalid(format!(
                    "pg_dsn setting {name:?} must declare exactly one of value or value_from"
                )));
            }
            Ok(PgDsnSetting {
                name,
                value: literal,
                value_from,
            })
        })
        .collect()
}

fn parse_pg_dsn_setting_value_from(
    value: Option<&TomlValue>,
) -> Result<Option<PgDsnSettingValueFrom>, ToolDiscoveryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value.as_table().ok_or_else(|| {
        ToolDiscoveryError::Invalid("pg_dsn setting value_from must be a table".to_owned())
    })?;
    let principal_label = optional_str(table, "principal_label").map(ToOwned::to_owned);
    let principal_field = optional_str(table, "principal_field").map(ToOwned::to_owned);
    let proxy_label = optional_str(table, "proxy_label").map(ToOwned::to_owned);
    let declared = [
        principal_label.as_ref(),
        principal_field.as_ref(),
        proxy_label.as_ref(),
    ]
    .into_iter()
    .filter(|value| value.is_some())
    .count();
    if declared != 1 {
        return Err(ToolDiscoveryError::Invalid(
            "pg_dsn setting value_from must declare exactly one of principal_label, principal_field, or proxy_label".to_owned(),
        ));
    }
    Ok(Some(PgDsnSettingValueFrom {
        principal_label,
        principal_field,
        proxy_label,
    }))
}

fn parse_aws_auth_secret(
    table: &toml::Table,
    name: String,
    labels: &BTreeMap<String, String>,
) -> Result<ToolSecret, ToolDiscoveryError> {
    // `hosts` is required (no tool-level fallback, matching the centaur-perms
    // loader); the credential refs are placeholders the proxy re-signs with.
    let hosts = required_string_array(table.get("hosts"), "hosts")?;
    let access_key_id_ref = required_str(table, "access_key_id")?.to_owned();
    let secret_access_key_ref = required_str(table, "secret_access_key")?.to_owned();
    let session_token_ref = match table.get("session_token") {
        None => None,
        Some(_) => Some(required_str(table, "session_token")?.to_owned()),
    };
    let allowed_regions = optional_string_array(table.get("allowed_regions"))?.unwrap_or_default();
    let allowed_services =
        optional_string_array(table.get("allowed_services"))?.unwrap_or_default();
    Ok(ToolSecret::AwsAuth(AwsAuthSecret {
        name,
        labels: labels.clone(),
        hosts,
        access_key_id_ref,
        secret_access_key_ref,
        session_token_ref,
        allowed_regions,
        allowed_services,
    }))
}

fn parse_oauth_fields(
    value: Option<&TomlValue>,
    secret_name: &str,
) -> Result<Vec<(String, OAuthFieldSource)>, ToolDiscoveryError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let table = value.as_table().ok_or_else(|| {
        ToolDiscoveryError::Invalid(format!(
            "oauth_token entry {secret_name:?} fields must be a table"
        ))
    })?;
    let mut fields = Vec::new();
    for (field_name, value) in table {
        let source = if let Some(secret_ref) = value.as_str() {
            OAuthFieldSource {
                secret_ref: nonempty(secret_ref, "oauth secret_ref")?.to_owned(),
                json_key: None,
            }
        } else {
            let table = value.as_table().ok_or_else(|| {
                ToolDiscoveryError::Invalid(format!(
                    "oauth field {field_name:?} must be a string or table"
                ))
            })?;
            OAuthFieldSource {
                secret_ref: required_str(table, "secret_ref")?.to_owned(),
                json_key: optional_str(table, "json_key").map(ToOwned::to_owned),
            }
        };
        fields.push((field_name.clone(), source));
    }
    fields.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(fields)
}

fn fragment_from_secrets(secrets: Vec<ToolSecret>) -> Result<ProxyFragment, ToolDiscoveryError> {
    let mut fragment = ProxyFragment::default();
    let http = http_secret_transform(&secrets)?;
    if let Some(transform) = http {
        fragment.transforms.push(transform);
    }
    fragment.transforms.extend(gcp_auth_transforms(&secrets)?);
    fragment
        .transforms
        .extend(gcp_id_token_transforms(&secrets)?);
    fragment.transforms.extend(aws_auth_transforms(&secrets)?);
    if let Some(transform) = oauth_token_transform(&secrets)? {
        fragment.transforms.push(transform);
    }
    fragment.postgres.extend(postgres_listeners(&secrets)?);
    Ok(fragment)
}

fn http_secret_transform(secrets: &[ToolSecret]) -> Result<Option<Transform>, ToolDiscoveryError> {
    let mut grouped =
        BTreeMap::<HttpSecretKey, (BTreeSet<String>, BTreeMap<String, String>)>::new();
    for secret in secrets {
        let ToolSecret::Http(secret) = secret else {
            continue;
        };
        let entry = grouped.entry(HttpSecretKey::from(secret)).or_default();
        entry.0.extend(secret.hosts.iter().cloned());
        merge_tool_labels(&mut entry.1, &secret.labels);
    }
    if grouped.is_empty() {
        return Ok(None);
    }
    let mut entries = Vec::new();
    for (key, (hosts, labels)) in grouped {
        let mut extra = BTreeMap::new();
        let mut entry = Secret {
            id: Some(key.name.clone()),
            source: Some(yaml_map([("placeholder", yaml_string(&key.secret_ref))])?),
            rules: host_rules(hosts)?,
            ..Default::default()
        };
        entry.extra.insert("labels".to_owned(), yaml_value(labels)?);
        match key.mode {
            HttpSecretMode::Replace => {
                extra.insert("match_headers".to_owned(), yaml_value(&key.match_headers)?);
                if key.match_path {
                    extra.insert("match_path".to_owned(), yaml_value(true)?);
                }
                if key.match_query {
                    extra.insert("match_query".to_owned(), yaml_value(true)?);
                }
                entry.replace = Some(SecretReplace {
                    proxy_value: Some(key.replacer),
                    extra,
                });
            }
            HttpSecretMode::Inject => {
                let mut inject = BTreeMap::new();
                if !key.inject_header.is_empty() {
                    inject.insert("header", yaml_string(&key.inject_header));
                }
                if !key.inject_formatter.is_empty() {
                    inject.insert("formatter", yaml_string(&key.inject_formatter));
                }
                if !key.inject_query_param.is_empty() {
                    inject.insert("query_param", yaml_string(&key.inject_query_param));
                }
                entry.inject = Some(yaml_value(inject)?);
            }
        }
        entries.push(entry);
    }
    Ok(Some(Transform {
        name: "secrets".to_owned(),
        config: TransformConfig {
            secrets: entries,
            ..Default::default()
        },
        ..Default::default()
    }))
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct HttpSecretKey {
    name: String,
    secret_ref: String,
    mode: HttpSecretMode,
    replacer: String,
    match_headers: Vec<String>,
    match_path: bool,
    match_query: bool,
    inject_header: String,
    inject_formatter: String,
    inject_query_param: String,
}

impl From<&HttpSecret> for HttpSecretKey {
    fn from(secret: &HttpSecret) -> Self {
        Self {
            name: secret.name.clone(),
            secret_ref: secret.secret_ref.clone(),
            mode: secret.mode.clone(),
            replacer: secret.replacer.clone(),
            match_headers: secret.match_headers.clone(),
            match_path: secret.match_path,
            match_query: secret.match_query,
            inject_header: secret.inject_header.clone(),
            inject_formatter: secret.inject_formatter.clone(),
            inject_query_param: secret.inject_query_param.clone(),
        }
    }
}

fn gcp_auth_transforms(secrets: &[ToolSecret]) -> Result<Vec<Transform>, ToolDiscoveryError> {
    let mut by_ref =
        BTreeMap::<String, (BTreeSet<String>, BTreeSet<String>, BTreeMap<String, String>)>::new();
    for secret in secrets {
        let ToolSecret::GcpAuth(secret) = secret else {
            continue;
        };
        let entry = by_ref.entry(secret.secret_ref.clone()).or_default();
        entry.0.extend(secret.hosts.iter().cloned());
        entry.1.extend(secret.scopes.iter().cloned());
        merge_tool_labels(&mut entry.2, &secret.labels);
    }
    let mut transforms = Vec::new();
    for (secret_ref, (hosts, scopes, labels)) in by_ref {
        let mut config = BTreeMap::new();
        config.insert(
            "keyfile".to_owned(),
            yaml_map([("placeholder", yaml_string(&secret_ref))])?,
        );
        config.insert(
            "scopes".to_owned(),
            yaml_value(if scopes.is_empty() {
                vec!["https://www.googleapis.com/auth/cloud-platform".to_owned()]
            } else {
                scopes.into_iter().collect()
            })?,
        );
        let hosts = if hosts.is_empty() {
            BTreeSet::from(["*.googleapis.com".to_owned()])
        } else {
            hosts
        };
        config.insert("rules".to_owned(), yaml_value(host_rules(hosts)?)?);
        config.insert("labels".to_owned(), yaml_value(labels)?);
        transforms.push(Transform {
            name: "gcp_auth".to_owned(),
            config: TransformConfig {
                extra: config,
                ..Default::default()
            },
            ..Default::default()
        });
    }
    Ok(transforms)
}

fn gcp_id_token_transforms(secrets: &[ToolSecret]) -> Result<Vec<Transform>, ToolDiscoveryError> {
    type GcpIdTokenKey = (String, String, Option<String>);
    let mut by_identity =
        BTreeMap::<GcpIdTokenKey, (BTreeSet<String>, BTreeMap<String, String>)>::new();
    for secret in secrets {
        let ToolSecret::GcpIdToken(secret) = secret else {
            continue;
        };
        let entry = by_identity
            .entry((
                secret.secret_ref.clone(),
                secret.audience.clone(),
                secret.header.clone(),
            ))
            .or_default();
        entry.0.extend(secret.hosts.iter().cloned());
        merge_tool_labels(&mut entry.1, &secret.labels);
    }

    let mut transforms = Vec::new();
    for ((secret_ref, audience, header), (hosts, labels)) in by_identity {
        let mut config = BTreeMap::new();
        config.insert(
            "keyfile".to_owned(),
            yaml_map([("placeholder", yaml_string(&secret_ref))])?,
        );
        config.insert("audience".to_owned(), yaml_string(&audience));
        if let Some(header) = &header {
            config.insert("header".to_owned(), yaml_string(header));
        }
        config.insert("rules".to_owned(), yaml_value(host_rules(hosts)?)?);
        config.insert("labels".to_owned(), yaml_value(labels)?);
        transforms.push(Transform {
            name: "gcp_id_token".to_owned(),
            config: TransformConfig {
                extra: config,
                ..Default::default()
            },
            ..Default::default()
        });
    }
    Ok(transforms)
}

fn aws_auth_transforms(secrets: &[ToolSecret]) -> Result<Vec<Transform>, ToolDiscoveryError> {
    // Group by credential identity (the placeholder refs) and union the host
    // rules + region/service scoping across tools that share the same keys, so
    // iron-control derives one stable `aws_auth` secret per credential (keyed on
    // the access-key placeholder) instead of colliding foreign_ids. Mirrors the
    // dedup `gcp_auth_transforms` does by secret_ref.
    type AwsCredKey = (String, String, Option<String>);
    let mut by_cred = BTreeMap::<
        AwsCredKey,
        (
            BTreeSet<String>,
            BTreeSet<String>,
            BTreeSet<String>,
            BTreeMap<String, String>,
        ),
    >::new();
    for secret in secrets {
        let ToolSecret::AwsAuth(secret) = secret else {
            continue;
        };
        let entry = by_cred
            .entry((
                secret.access_key_id_ref.clone(),
                secret.secret_access_key_ref.clone(),
                secret.session_token_ref.clone(),
            ))
            .or_default();
        entry.0.extend(secret.hosts.iter().cloned());
        entry.1.extend(secret.allowed_regions.iter().cloned());
        entry.2.extend(secret.allowed_services.iter().cloned());
        merge_tool_labels(&mut entry.3, &secret.labels);
    }
    let mut transforms = Vec::new();
    for (
        (access_key_id_ref, secret_access_key_ref, session_token_ref),
        (hosts, regions, services, labels),
    ) in by_cred
    {
        let mut config = BTreeMap::new();
        config.insert(
            "access_key_id".to_owned(),
            yaml_map([("placeholder", yaml_string(&access_key_id_ref))])?,
        );
        config.insert(
            "secret_access_key".to_owned(),
            yaml_map([("placeholder", yaml_string(&secret_access_key_ref))])?,
        );
        if let Some(session_token_ref) = &session_token_ref {
            config.insert(
                "session_token".to_owned(),
                yaml_map([("placeholder", yaml_string(session_token_ref))])?,
            );
        }
        if !regions.is_empty() {
            config.insert(
                "allowed_regions".to_owned(),
                yaml_value(regions.into_iter().collect::<Vec<_>>())?,
            );
        }
        if !services.is_empty() {
            config.insert(
                "allowed_services".to_owned(),
                yaml_value(services.into_iter().collect::<Vec<_>>())?,
            );
        }
        config.insert("rules".to_owned(), yaml_value(host_rules(hosts)?)?);
        config.insert("labels".to_owned(), yaml_value(labels)?);
        transforms.push(Transform {
            name: "aws_auth".to_owned(),
            config: TransformConfig {
                extra: config,
                ..Default::default()
            },
            ..Default::default()
        });
    }
    Ok(transforms)
}

fn oauth_token_transform(secrets: &[ToolSecret]) -> Result<Option<Transform>, ToolDiscoveryError> {
    let mut tokens = Vec::new();
    for secret in secrets {
        let ToolSecret::OAuthToken(secret) = secret else {
            continue;
        };
        let mut token = BTreeMap::new();
        token.insert("grant".to_owned(), yaml_string(&secret.grant));
        token.insert("labels".to_owned(), yaml_value(&secret.labels)?);
        for (field_name, source) in &secret.fields {
            token.insert(field_name.clone(), oauth_field_source(source)?);
        }
        token.insert(
            "rules".to_owned(),
            yaml_value(host_rules_set(&secret.hosts)?)?,
        );
        if !secret.scopes.is_empty() {
            token.insert("scopes".to_owned(), yaml_value(&secret.scopes)?);
        }
        if let Some(token_endpoint) = &secret.token_endpoint {
            token.insert("token_endpoint".to_owned(), yaml_string(token_endpoint));
        }
        if let Some(audience) = &secret.audience {
            token.insert("audience".to_owned(), yaml_string(audience));
        }
        if !secret.token_endpoint_headers.is_empty() {
            let mut headers = BTreeMap::new();
            for (header_name, source) in &secret.token_endpoint_headers {
                headers.insert(header_name.clone(), oauth_field_source(source)?);
            }
            token.insert("token_endpoint_headers".to_owned(), yaml_value(headers)?);
        }
        tokens.push(yaml_value(token)?);
    }
    if tokens.is_empty() {
        return Ok(None);
    }
    let mut extra = BTreeMap::new();
    extra.insert("tokens".to_owned(), YamlValue::Sequence(tokens));
    Ok(Some(Transform {
        name: "oauth_token".to_owned(),
        config: TransformConfig {
            extra,
            ..Default::default()
        },
        ..Default::default()
    }))
}

fn postgres_listeners(secrets: &[ToolSecret]) -> Result<Vec<PostgresListener>, ToolDiscoveryError> {
    let mut by_name = BTreeMap::<String, PgDsnSecret>::new();
    for secret in secrets {
        let ToolSecret::PgDsn(secret) = secret else {
            continue;
        };
        by_name
            .entry(secret.name.clone())
            .and_modify(|existing| merge_tool_labels(&mut existing.labels, &secret.labels))
            .or_insert(secret.clone());
    }
    by_name
        .into_values()
        .map(|secret| {
            let mut extra = BTreeMap::new();
            if let Some(role) = &secret.role {
                extra.insert("role".to_owned(), yaml_string(role));
            }
            extra.insert("labels".to_owned(), yaml_value(&secret.labels)?);
            Ok(PostgresListener {
                name: Some(secret.name.to_lowercase()),
                upstream: Some(PostgresUpstream {
                    dsn: Some(yaml_map([(
                        "placeholder",
                        yaml_string(&secret.secret_ref),
                    )])?),
                    ..Default::default()
                }),
                // Retain the declared DSN env var name + database as an
                // api-rs-internal annotation (`skip_serializing`) for older
                // fragment consumers. The shared Centaur Postgres route is now
                // injected by the sandbox backend independently of tool
                // discovery.
                sandbox_env: Some(SandboxEnv {
                    name: Some(secret.name.clone()),
                    database: Some(secret.database.clone()),
                    ..Default::default()
                }),
                settings: secret.settings.clone(),
                extra,
                ..Default::default()
            })
        })
        .collect()
}

fn oauth_field_source(source: &OAuthFieldSource) -> Result<YamlValue, ToolDiscoveryError> {
    let mut value = BTreeMap::new();
    value.insert("placeholder", yaml_string(&source.secret_ref));
    if let Some(json_key) = &source.json_key {
        value.insert("json_key", yaml_string(json_key));
    }
    yaml_value(value)
}

fn host_rules(hosts: BTreeSet<String>) -> Result<Vec<YamlValue>, ToolDiscoveryError> {
    hosts
        .into_iter()
        .map(|host| yaml_map([("host", yaml_string(&host))]))
        .collect()
}

fn host_rules_set(hosts: &[String]) -> Result<Vec<YamlValue>, ToolDiscoveryError> {
    host_rules(hosts.iter().cloned().collect())
}

fn yaml_string(value: &str) -> YamlValue {
    YamlValue::String(value.to_owned())
}

fn yaml_map<const N: usize>(
    values: [(&str, YamlValue); N],
) -> Result<YamlValue, ToolDiscoveryError> {
    let map = values
        .into_iter()
        .map(|(key, value)| (YamlValue::String(key.to_owned()), value))
        .collect();
    Ok(YamlValue::Mapping(map))
}

fn yaml_value<T: Serialize>(value: T) -> Result<YamlValue, ToolDiscoveryError> {
    serde_yaml::to_value(value).map_err(ToolDiscoveryError::Serialize)
}

fn required_str<'a>(table: &'a toml::Table, key: &str) -> Result<&'a str, ToolDiscoveryError> {
    let value = table
        .get(key)
        .and_then(TomlValue::as_str)
        .ok_or_else(|| ToolDiscoveryError::Invalid(format!("missing required string {key:?}")))?;
    nonempty(value, key)
}

fn optional_str<'a>(table: &'a toml::Table, key: &str) -> Option<&'a str> {
    table
        .get(key)
        .and_then(TomlValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn nonempty<'a>(value: &'a str, key: &str) -> Result<&'a str, ToolDiscoveryError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(ToolDiscoveryError::Invalid(format!(
            "{key} must be non-empty"
        )));
    }
    Ok(value)
}

fn string_array(value: Option<&TomlValue>) -> Vec<String> {
    optional_string_array(value)
        .unwrap_or_default()
        .unwrap_or_default()
}

fn required_string_array(
    value: Option<&TomlValue>,
    key: &str,
) -> Result<Vec<String>, ToolDiscoveryError> {
    optional_string_array(value)?.ok_or_else(|| {
        ToolDiscoveryError::Invalid(format!("missing required string array {key:?}"))
    })
}

fn optional_string_array(
    value: Option<&TomlValue>,
) -> Result<Option<Vec<String>>, ToolDiscoveryError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| ToolDiscoveryError::Invalid("expected string array".to_owned()))?;
    let mut out = Vec::new();
    for item in array {
        let Some(value) = item
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Err(ToolDiscoveryError::Invalid(
                "expected non-empty string array".to_owned(),
            ));
        };
        out.push(value.to_owned());
    }
    Ok(Some(out))
}

fn optional_bool(table: &toml::Table, key: &str) -> Result<Option<bool>, ToolDiscoveryError> {
    match table.get(key) {
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| ToolDiscoveryError::Invalid(format!("{key:?} must be boolean"))),
        None => Ok(None),
    }
}

fn validate_gcp_id_token_header(value: String) -> Result<String, ToolDiscoveryError> {
    normalize_gcp_id_token_header(&value).ok_or_else(|| {
        ToolDiscoveryError::Invalid(format!(
            "gcp_id_token header must be one of {}, got {value:?}",
            GCP_ID_TOKEN_ALLOWED_HEADERS.join(", ")
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn resolves_tool_dirs_from_explicit_env_string() {
        let config = ToolDiscoveryConfig {
            tool_dirs: Some("/base:/overlay".to_owned()),
            tools_path: Some(PathBuf::from("/ignored")),
            ..Default::default()
        };

        assert_eq!(
            config.resolve_tool_dirs().unwrap(),
            vec![PathBuf::from("/base"), PathBuf::from("/overlay")]
        );
    }

    #[test]
    fn resolves_public_tool_dirs_from_explicit_env_string() {
        let config = ToolDiscoveryConfig {
            public_tool_dirs: Some("/public-base:/public-overlay".to_owned()),
            ..Default::default()
        };

        assert_eq!(
            config.resolve_public_tool_dirs(),
            vec![
                PathBuf::from("/public-base"),
                PathBuf::from("/public-overlay")
            ]
        );
    }

    #[test]
    fn resolves_sandbox_style_tools_path_and_overlay_path() {
        let config = ToolDiscoveryConfig {
            tools_path: Some(PathBuf::from("/base")),
            tools_overlay_path: Some(PathBuf::from("/overlay")),
            ..Default::default()
        };

        assert_eq!(
            config.resolve_tool_dirs().unwrap(),
            vec![PathBuf::from("/base"), PathBuf::from("/overlay")]
        );
    }

    #[test]
    fn postgres_listeners_retain_sandbox_env_name_and_database() {
        // api-rs bakes the sandbox PG DSNs from `sandbox_env`, so the listener
        // must carry the tool's declared env var name and database verbatim.
        let listeners = postgres_listeners(&[ToolSecret::PgDsn(PgDsnSecret {
            name: "RESHIFT_DSN".to_owned(),
            secret_ref: "RESHIFT_DSN".to_owned(),
            labels: tool_labels("company_context", "centaur"),
            database: "warehouse".to_owned(),
            role: Some("centaur_slack_reader".to_owned()),
            settings: vec![
                PgDsnSetting {
                    name: "centaur.slack_channel_id".to_owned(),
                    value: None,
                    value_from: Some(PgDsnSettingValueFrom {
                        principal_label: Some("slack_channel_id".to_owned()),
                        principal_field: None,
                        proxy_label: None,
                    }),
                },
                PgDsnSetting {
                    name: "centaur.slack_user_id".to_owned(),
                    value: None,
                    value_from: Some(PgDsnSettingValueFrom {
                        principal_label: None,
                        principal_field: None,
                        proxy_label: Some("centaur.slack_user_id".to_owned()),
                    }),
                },
            ],
        })])
        .unwrap();

        let sandbox_env = listeners[0].sandbox_env.as_ref().unwrap();
        assert_eq!(sandbox_env.name.as_deref(), Some("RESHIFT_DSN"));
        assert_eq!(sandbox_env.database.as_deref(), Some("warehouse"));
        assert_eq!(
            listeners[0].extra.get("role").and_then(YamlValue::as_str),
            Some("centaur_slack_reader")
        );
        assert_eq!(listeners[0].settings.len(), 2);
        assert_eq!(listeners[0].settings[0].name, "centaur.slack_channel_id");
        assert_eq!(listeners[0].settings[1].name, "centaur.slack_user_id");
        assert_eq!(
            listeners[0].settings[1]
                .value_from
                .as_ref()
                .and_then(|value_from| value_from.proxy_label.as_deref()),
            Some("centaur.slack_user_id")
        );
    }

    #[test]
    fn rejects_pg_dsn_value_from_with_multiple_selectors() {
        let value: TomlValue = toml::from_str(
            r#"
database = "warehouse"
settings = [
  { name = "centaur.slack_user_id", value_from = { principal_label = "slack_user_id", proxy_label = "centaur.slack_user_id" } }
]
"#,
        )
        .unwrap();
        let err = parse_pg_dsn_secret(
            value.as_table().unwrap(),
            "RESHIFT_DSN".to_owned(),
            "RESHIFT_DSN".to_owned(),
            &BTreeMap::new(),
        )
        .unwrap_err();

        assert!(
            err.to_string().contains("must declare exactly one"),
            "{err}"
        );
    }

    #[test]
    fn discovers_http_oauth_and_overlay_shadowing() {
        let temp = temp_dir("api-rs-tools");
        let base = temp.join("base");
        let overlay = temp.join("overlay");
        write_tool(
            &base.join("category").join("alpha"),
            r#"
[project]
description = "base alpha"

[tool.centaur]
secrets = [{type = "http", name = "BASE_TOKEN", match_headers = ["Authorization"], hosts = ["api.base.test"]}]
"#,
        );
        write_tool(
            &overlay.join("alpha"),
            r#"
[project]
description = "overlay alpha"

[tool.centaur]
secrets = [{type = "http", name = "OVERLAY_TOKEN", match_query = true, hosts = ["api.overlay.test"]}]
"#,
        );
        write_tool(
            &base.join("gsuite"),
            r#"
[project]
description = "oauth"

[tool.centaur]
secrets = [
  {type = "oauth_token", grant = "refresh_token", name = "GOOGLE_TOKEN_JSON", token_endpoint = "https://oauth2.googleapis.com/token", hosts = ["gmail.googleapis.com"], fields = { refresh_token = { secret_ref = "GOOGLE_TOKEN_JSON", json_key = "refresh_token" }, client_id = { secret_ref = "GOOGLE_TOKEN_JSON", json_key = "client_id" } } },
]
"#,
        );

        let discovered = discover_tool_proxy_fragment(&[base.clone(), overlay.clone()]).unwrap();

        assert_eq!(discovered.tool_count, 2);
        assert_eq!(discovered.secret_count, 2);
        let secrets = discovered.fragment.transforms[0].config.secrets.clone();
        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].id.as_deref(), Some("OVERLAY_TOKEN"));
        let labels = secrets[0]
            .extra
            .get("labels")
            .and_then(YamlValue::as_mapping)
            .expect("static secret labels");
        assert_eq!(
            labels
                .get(YamlValue::String("centaur-tool".to_owned()))
                .and_then(YamlValue::as_str),
            Some("alpha")
        );
        assert_eq!(
            labels
                .get(YamlValue::String("centaur-tool-overlay".to_owned()))
                .and_then(YamlValue::as_str),
            Some("overlay")
        );
        assert_eq!(
            discovered.fragment.transforms[1].name,
            "oauth_token".to_owned()
        );
        let tokens = discovered.fragment.transforms[1].config.extra["tokens"]
            .as_sequence()
            .expect("oauth tokens");
        assert_eq!(tokens[0]["labels"]["centaur-tool"].as_str(), Some("gsuite"));

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn discovers_personas_with_overlay_shadowing_and_default_validation() {
        let temp = temp_dir("api-rs-personas");
        let base = temp.join("base");
        let overlay = temp.join("overlay");
        write_persona(&base.join("personas").join("eng"), "base prompt");
        write_persona(&overlay.join("personas").join("eng"), "overlay prompt");
        write_persona(&overlay.join("personas").join("ops"), "ops prompt");

        let registry =
            discover_persona_registry(&[base.clone(), overlay.clone()], Some("eng".to_owned()))
                .unwrap();
        let personas = registry.summaries();

        assert_eq!(personas.len(), 2);
        let eng = personas
            .iter()
            .find(|persona| persona.id == "eng")
            .expect("eng persona");
        assert_eq!(eng.source_root, overlay.display().to_string());
        assert!(eng.source_path.ends_with("personas/eng"));
        assert_ne!(
            eng.prompt_hash,
            "sha256:c41ac32f8b086eecbd1c70d06689eb428de2a2c740d086640851985f26c4e2fc"
        );
        assert_eq!(
            eng.prompt_hash,
            "sha256:af70f573f4496a1cf92865966cb522c2c142a5789e075660a56bea66080bc738"
        );
        assert!(
            discover_persona_registry(&[base.clone(), overlay.clone()], Some("missing".to_owned()))
                .is_err()
        );

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn discovers_aws_auth_tool_as_transform() {
        let temp = temp_dir("api-rs-tools-aws");
        let base = temp.join("base");
        // Mirrors tools/infra/cloudwatch/pyproject.toml.
        write_tool(
            &base.join("infra").join("cloudwatch"),
            r#"
[project]
description = "cloudwatch"

[tool.centaur]
hosts = ["logs.*.amazonaws.com", "monitoring.*.amazonaws.com"]
secrets = [
  { type = "aws_auth", name = "cloudwatch", access_key_id = "AWS_ACCESS_KEY_ID", secret_access_key = "AWS_SECRET_ACCESS_KEY", hosts = ["logs.*.amazonaws.com", "monitoring.*.amazonaws.com"], allowed_services = ["logs", "monitoring"] },
]
"#,
        );

        let discovered = discover_tool_proxy_fragment(std::slice::from_ref(&base)).unwrap();

        // The tool is kept (not dropped) and contributes exactly one aws_auth
        // transform in the shape iron-control's translator consumes.
        assert_eq!(discovered.tool_count, 1);
        assert_eq!(discovered.secret_count, 1);
        let transform = discovered
            .fragment
            .transforms
            .iter()
            .find(|transform| transform.name == "aws_auth")
            .expect("aws_auth transform present");
        let config = &transform.config.extra;
        assert_eq!(
            config["access_key_id"]["placeholder"].as_str(),
            Some("AWS_ACCESS_KEY_ID")
        );
        assert_eq!(
            config["secret_access_key"]["placeholder"].as_str(),
            Some("AWS_SECRET_ACCESS_KEY")
        );
        assert!(!config.contains_key("session_token"));
        assert!(!config.contains_key("allowed_regions"));
        assert_eq!(
            config["allowed_services"]
                .as_sequence()
                .unwrap()
                .iter()
                .filter_map(YamlValue::as_str)
                .collect::<Vec<_>>(),
            vec!["logs", "monitoring"]
        );
        assert_eq!(config["rules"].as_sequence().unwrap().len(), 2);

        // No sandbox env rides along: the tool embeds its own throwaway SigV4
        // credentials, so aws_auth contributes nothing to placeholder env.
        let placeholders =
            centaur_iron_proxy::placeholder_env(std::slice::from_ref(&discovered.fragment));
        assert!(placeholders.is_empty());

        let _ = fs::remove_dir_all(temp);
    }

    #[test]
    fn discovers_gcp_id_token_tool_as_transform() {
        let temp = temp_dir("api-rs-tools-gcp-id-token");
        let base = temp.join("base");
        write_tool(
            &base.join("infra").join("cloudrun"),
            r#"
[project]
description = "cloudrun"

[tool.centaur]
secrets = [
  { type = "gcp_id_token", name = "CLOUD_RUN_KEYFILE", audience = "https://my-service-abc123-uc.a.run.app", header = "X-Serverless-Authorization", hosts = ["my-service-abc123-uc.a.run.app"] },
]
"#,
        );

        let discovered = discover_tool_proxy_fragment(std::slice::from_ref(&base)).unwrap();

        assert_eq!(discovered.tool_count, 1);
        assert_eq!(discovered.secret_count, 1);
        let transform = discovered
            .fragment
            .transforms
            .iter()
            .find(|transform| transform.name == "gcp_id_token")
            .expect("gcp_id_token transform present");
        let config = &transform.config.extra;
        assert_eq!(
            config["keyfile"]["placeholder"].as_str(),
            Some("CLOUD_RUN_KEYFILE")
        );
        assert_eq!(
            config["audience"].as_str(),
            Some("https://my-service-abc123-uc.a.run.app")
        );
        assert_eq!(
            config["header"].as_str(),
            Some("x-serverless-authorization")
        );
        assert_eq!(config["rules"].as_sequence().unwrap().len(), 1);
        assert_eq!(config["labels"]["centaur-tool"].as_str(), Some("cloudrun"));

        let placeholders =
            centaur_iron_proxy::placeholder_env(std::slice::from_ref(&discovered.fragment));
        assert!(placeholders.is_empty());

        let _ = fs::remove_dir_all(temp);
    }

    fn temp_dir(prefix: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("{prefix}-{}-{suffix}", std::process::id()))
    }

    fn write_tool(path: &Path, pyproject: &str) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("pyproject.toml"), pyproject).unwrap();
    }

    fn write_persona(path: &Path, prompt: &str) {
        fs::create_dir_all(path).unwrap();
        fs::write(
            path.join("pyproject.toml"),
            r#"
[project]
description = "persona"

[tool.centaur]
type = "persona"
"#,
        )
        .unwrap();
        fs::write(path.join("PROMPT.md"), prompt).unwrap();
    }
}
