//! Tool discovery and ``pyproject.toml`` `[tool.centaur]` secret parsing.
//!
//! This is the CLI-side analogue of the API's `ToolManager._collect_tools` and
//! `_parse_secret` (`services/api/api/tool_manager.py`). It resolves a tool by
//! name across one or more (overlay-ordered) tool directories — later
//! directories shadow earlier ones, exactly like the API — and parses the
//! tool's declared secrets into a neutral [`ParsedSecret`] the translator turns
//! into iron-control inputs. Only the secret *schema* is reimplemented here;
//! the API's loader stays the source of truth for runtime tool loading.

use std::path::{Path, PathBuf};

use centaur_iron_control::{GCP_ID_TOKEN_ALLOWED_HEADERS, normalize_gcp_id_token_header};
use centaur_iron_proxy::{PgDsnSetting, PgDsnSettingValueFrom};
use eyre::{Context, Result, bail, eyre};
use toml::Value;

/// Headers the legacy raw-string shim scans for a replace-mode placeholder,
/// mirroring `DEFAULT_MATCH_HEADERS` in `tool_manager.py`. Typed entries name
/// their own `match_headers` instead and never fall back to this set.
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

/// Enums iron-proxy's `hmac_sign` transform accepts, mirroring `_HMAC_*` in
/// `tool_manager.py`. Centralized so parser errors list the same options.
const HMAC_ALGORITHMS: &[&str] = &["sha256", "sha512", "sha1"];
const HMAC_KEY_ENCODINGS: &[&str] = &["raw", "base64", "hex"];
const HMAC_OUTPUT_ENCODINGS: &[&str] = &["base64", "hex"];
const HMAC_TIMESTAMP_FORMATS: &[&str] = &["unix_seconds", "unix_millis", "unix_nanos", "rfc3339"];
/// The required `credentials` entry: the HMAC key. Other keys are user-named
/// and only referenced from `headers[].value` templates.
const HMAC_REQUIRED_CREDENTIAL: &str = "secret";

/// Per-grant `oauth_token` credential fields: `(grant, required, optional)`,
/// mirroring `_OAUTH_GRANT_FIELDS`.
const OAUTH_GRANT_FIELDS: &[(&str, &[&str], &[&str])] = &[
    (
        "refresh_token",
        &["refresh_token", "client_id"],
        &["client_secret"],
    ),
    ("client_credentials", &["client_id", "client_secret"], &[]),
    (
        "password",
        &["username", "password", "client_id"],
        &["client_secret"],
    ),
    (
        "jwt_bearer",
        &["issuer", "subject", "private_key"],
        &["private_key_id"],
    ),
];

/// How an HTTP credential rides on the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretMode {
    /// The tool writes `replacer`; iron-proxy swaps it for the resolved value.
    Replace,
    /// iron-proxy adds the credential itself; the tool never sees it.
    Inject,
}

/// One credential field's source: a `secret_ref`, optionally pulling a single
/// `json_key` out of a JSON-encoded secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSource {
    pub secret_ref: String,
    pub json_key: Option<String>,
}

/// A `type = "http"` secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpSecret {
    pub name: String,
    pub secret_ref: String,
    pub mode: SecretMode,
    pub hosts: Vec<String>,
    // replace mode
    pub replacer: String,
    pub match_headers: Vec<String>,
    pub match_path: bool,
    pub match_query: bool,
    // inject mode
    pub inject_header: Option<String>,
    pub inject_formatter: Option<String>,
    pub inject_query_param: Option<String>,
}

/// A `type = "oauth_token"` secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthTokenSecret {
    pub name: String,
    pub grant: String,
    pub hosts: Vec<String>,
    pub fields: Vec<(String, FieldSource)>,
    pub scopes: Vec<String>,
    pub token_endpoint: Option<String>,
    pub token_endpoint_headers: Vec<(String, FieldSource)>,
    pub audience: Option<String>,
}

/// A `type = "gcp_auth"` secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcpAuthSecret {
    pub name: String,
    pub secret_ref: String,
    pub hosts: Vec<String>,
    pub scopes: Vec<String>,
}

/// A `type = "gcp_id_token"` secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcpIdTokenSecret {
    pub name: String,
    pub secret_ref: String,
    pub hosts: Vec<String>,
    pub audience: String,
    pub header: Option<String>,
}

/// A `type = "pg_dsn"` secret: a Postgres upstream the proxy fronts. `name` is
/// the DSN env var the sandbox reads, `secret_ref` resolves the upstream
/// connection string, and `database` is the database to connect to. pg_dsn
/// secrets have no `hosts`/rules — a listener matches by port, not by request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgDsnSecret {
    pub name: String,
    pub secret_ref: String,
    pub database: String,
    pub role: Option<String>,
    pub settings: Vec<PgDsnSetting>,
}

/// One header iron-proxy's `hmac_sign` transform writes onto the upstream
/// request. `value` is a Go template evaluated against the signing context
/// (`.Timestamp`, `.Signature`, `.Credentials.<name>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HmacHeader {
    pub name: String,
    pub value: String,
}

/// A `type = "hmac_sign"` secret: a per-request HMAC signature iron-proxy mints
/// and writes onto the upstream request. `credentials` maps a name to its
/// source; the entry named `secret` is the HMAC key and is required. The other
/// keys are user-named and referenced from `headers[].value` templates as
/// `{{.Credentials.<name>}}`. The credentials and signing key never reach the
/// sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HmacSignSecret {
    pub name: String,
    pub hosts: Vec<String>,
    pub credentials: Vec<(String, FieldSource)>,
    pub headers: Vec<HmacHeader>,
    pub algorithm: String,
    pub key_encoding: String,
    pub output_encoding: String,
    pub message: String,
    pub timestamp_format: String,
    pub allow_chunked_body: bool,
}

/// A `type = "brokered_token"` secret: the *consumer* side of an iron-control
/// broker credential. The broker credential itself (the OAuth refresh loop) is
/// provisioned out of band via `centaur-perms broker create`; this entry just
/// registers a static secret that injects the broker's current access token.
/// `credential` is the broker credential's `foreign_id` to reference (defaults
/// to `name`); `inject_header`/`inject_formatter` default to an
/// `Authorization: Bearer {{.Value}}` injection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerTokenSecret {
    pub name: String,
    pub credential: String,
    pub hosts: Vec<String>,
    pub inject_header: String,
    pub inject_formatter: String,
}

/// A `type = "aws_auth"` secret: AWS SigV4 re-signing handled by iron-proxy's
/// `aws_auth` transform. The tool's AWS SDK signs each request with throwaway
/// *placeholder* credentials; iron-proxy reads the region/service from the
/// inbound signature's credential scope, strips it, and re-signs with the real
/// credentials resolved from `access_key_id_ref`/`secret_access_key_ref` (and
/// optional `session_token_ref` for STS). The real keys never reach the sandbox.
/// `allowed_regions`/`allowed_services` scope which regions/services the proxy
/// will sign for; `hosts` becomes the iron-control request rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwsAuthSecret {
    pub name: String,
    pub hosts: Vec<String>,
    pub access_key_id_ref: String,
    pub secret_access_key_ref: String,
    pub session_token_ref: Option<String>,
    pub allowed_regions: Vec<String>,
    pub allowed_services: Vec<String>,
}

/// One parsed `[tool.centaur]` secret entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedSecret {
    Http(HttpSecret),
    OAuthToken(OAuthTokenSecret),
    GcpAuth(GcpAuthSecret),
    GcpIdToken(GcpIdTokenSecret),
    PgDsn(PgDsnSecret),
    Hmac(HmacSignSecret),
    BrokerToken(BrokerTokenSecret),
    AwsAuth(AwsAuthSecret),
}

impl ParsedSecret {
    /// The declared secret name (e.g. `SLACK_BOT_TOKEN`).
    pub fn name(&self) -> &str {
        match self {
            ParsedSecret::Http(s) => &s.name,
            ParsedSecret::OAuthToken(s) => &s.name,
            ParsedSecret::GcpAuth(s) => &s.name,
            ParsedSecret::GcpIdToken(s) => &s.name,
            ParsedSecret::PgDsn(s) => &s.name,
            ParsedSecret::Hmac(s) => &s.name,
            ParsedSecret::BrokerToken(s) => &s.name,
            ParsedSecret::AwsAuth(s) => &s.name,
        }
    }
}

/// A resolved tool: its name, directory, and declared secrets.
#[derive(Debug, Clone)]
pub struct ToolManifest {
    pub name: String,
    pub dir: PathBuf,
    pub secrets: Vec<ParsedSecret>,
    pub optional_secrets: Vec<ParsedSecret>,
}

impl ToolManifest {
    /// Required then optional secrets, in declaration order.
    pub fn all_secrets(&self) -> impl Iterator<Item = &ParsedSecret> {
        self.secrets.iter().chain(self.optional_secrets.iter())
    }
}

/// Resolve the ordered tool directories from explicit `--tools-dir` values and
/// the colon-separated `TOOL_DIRS` env var (explicit values first, then env).
/// Later directories shadow earlier ones, matching the API's overlay order.
pub fn resolve_tool_dirs(cli_dirs: &[PathBuf], tool_dirs_env: Option<&str>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = cli_dirs.to_vec();
    if let Some(env) = tool_dirs_env {
        for entry in env.split(':').map(str::trim).filter(|s| !s.is_empty()) {
            dirs.push(PathBuf::from(entry));
        }
    }
    dirs
}

/// Find a tool by directory name across `dirs`, honoring overlay shadowing
/// (later dirs win) and one level of category subdirectories — the same scan
/// `_collect_tools` performs. Errors if the name is not found in any dir.
pub fn find_tool(dirs: &[PathBuf], name: &str) -> Result<ToolManifest> {
    if dirs.is_empty() {
        bail!("no tool directories configured; pass --tools-dir or set TOOL_DIRS");
    }
    let mut matched: Option<PathBuf> = None;
    for base in dirs {
        for candidate in collect_candidate_dirs(base) {
            if candidate.file_name().and_then(|n| n.to_str()) == Some(name)
                && candidate.join("pyproject.toml").is_file()
            {
                matched = Some(candidate);
            }
        }
    }
    let tool_dir = matched.ok_or_else(|| {
        eyre!(
            "tool {name:?} not found under any of: {}",
            dirs.iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    parse_manifest(&tool_dir)
}

/// Derive the operator-facing overlay identity for the root that supplied a
/// tool manifest. A root ending in `tools` is treated as a repo/overlay tools
/// directory, so `/repo/centaur/tools` becomes `centaur`.
pub fn overlay_name_for_tool_dir(tool_dir: &Path, dirs: &[PathBuf]) -> String {
    let matching_root = dirs
        .iter()
        .filter(|root| tool_dir.starts_with(root))
        .max_by_key(|root| root.components().count());
    matching_root
        .map(|root| overlay_name_for_root(root))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn overlay_name_for_root(root: &Path) -> String {
    let candidate = if root.file_name().and_then(|n| n.to_str()) == Some("tools") {
        root.parent().and_then(|p| p.file_name())
    } else {
        root.file_name()
    };
    candidate
        .and_then(|n| n.to_str())
        .map(centaur_iron_control::slugify)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Candidate tool directories under `base`: direct children carrying a
/// `pyproject.toml`, plus the children of category folders (one level deep).
fn collect_candidate_dirs(base: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let Ok(entries) = std::fs::read_dir(base) else {
        return candidates;
    };
    let mut children: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && !hidden(p))
        .collect();
    children.sort();
    for child in children {
        if child.join("pyproject.toml").is_file() {
            candidates.push(child);
        } else if let Ok(sub_entries) = std::fs::read_dir(&child) {
            let mut subs: Vec<PathBuf> = sub_entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_dir() && !hidden(p))
                .collect();
            subs.sort();
            candidates.extend(subs);
        }
    }
    candidates
}

fn hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.') || n.starts_with('_'))
}

/// Parse a tool directory's `pyproject.toml` `[tool.centaur]` block.
pub fn parse_manifest(tool_dir: &Path) -> Result<ToolManifest> {
    let name = tool_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| eyre!("tool dir {} has no name", tool_dir.display()))?
        .to_owned();
    let path = tool_dir.join("pyproject.toml");
    let text =
        std::fs::read_to_string(&path).wrap_err_with(|| format!("reading {}", path.display()))?;
    let doc: Value = text
        .parse::<Value>()
        .wrap_err_with(|| format!("parsing {}", path.display()))?;
    let centaur = doc
        .get("tool")
        .and_then(|t| t.get("centaur"))
        .and_then(Value::as_table);
    let Some(centaur) = centaur else {
        return Ok(ToolManifest {
            name,
            dir: tool_dir.to_owned(),
            secrets: vec![],
            optional_secrets: vec![],
        });
    };
    let default_hosts = str_array(centaur.get("hosts")).unwrap_or_default();
    let secrets = parse_secret_list(centaur.get("secrets"), &default_hosts)
        .wrap_err_with(|| format!("in {} [tool.centaur].secrets", path.display()))?;
    let optional_secrets = parse_secret_list(centaur.get("optional_secrets"), &default_hosts)
        .wrap_err_with(|| format!("in {} [tool.centaur].optional_secrets", path.display()))?;
    Ok(ToolManifest {
        name,
        dir: tool_dir.to_owned(),
        secrets,
        optional_secrets,
    })
}

fn parse_secret_list(
    entries: Option<&Value>,
    default_hosts: &[String],
) -> Result<Vec<ParsedSecret>> {
    let Some(entries) = entries else {
        return Ok(vec![]);
    };
    let arr = entries
        .as_array()
        .ok_or_else(|| eyre!("'secrets'/'optional_secrets' must be an array"))?;
    arr.iter().map(|e| parse_secret(e, default_hosts)).collect()
}

/// Port of `_parse_secret`: normalize one entry into a [`ParsedSecret`].
pub fn parse_secret(entry: &Value, default_hosts: &[String]) -> Result<ParsedSecret> {
    // Legacy raw-string shim: a bare name is a replace-mode HTTP secret that
    // scans the default header set and inherits the tool-level hosts.
    if let Some(s) = entry.as_str() {
        if s.is_empty() {
            bail!("secret entry string must be non-empty");
        }
        if default_hosts.is_empty() {
            bail!(
                "secret entry {s:?} requires the tool to declare non-empty top-level \
                 'hosts'; a secret without hosts would be unscoped in iron-proxy"
            );
        }
        return Ok(ParsedSecret::Http(HttpSecret {
            name: s.to_owned(),
            secret_ref: s.to_owned(),
            mode: SecretMode::Replace,
            hosts: default_hosts.to_vec(),
            replacer: s.to_owned(),
            match_headers: DEFAULT_MATCH_HEADERS
                .iter()
                .map(|h| (*h).to_owned())
                .collect(),
            match_path: false,
            match_query: false,
            inject_header: None,
            inject_formatter: None,
            inject_query_param: None,
        }));
    }
    let table = entry
        .as_table()
        .ok_or_else(|| eyre!("secret entry must be a string or table"))?;
    let name = req_str(table, "name").wrap_err("secret entry missing 'name'")?;
    // `header` is a deprecated alias for `http`.
    let secret_type = table.get("type").and_then(Value::as_str).unwrap_or("http");
    let secret_ref = match table.get("secret_ref") {
        Some(v) => v
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| eyre!("secret entry {name:?} has invalid 'secret_ref'"))?
            .to_owned(),
        None => name.clone(),
    };
    match secret_type {
        "http" | "header" => Ok(ParsedSecret::Http(parse_http(
            table,
            &name,
            &secret_ref,
            default_hosts,
        )?)),
        "oauth_token" => Ok(ParsedSecret::OAuthToken(parse_oauth(table, &name)?)),
        "gcp_auth" => Ok(ParsedSecret::GcpAuth(parse_gcp(table, &name, &secret_ref)?)),
        "gcp_id_token" => Ok(ParsedSecret::GcpIdToken(parse_gcp_id_token(
            table,
            &name,
            &secret_ref,
        )?)),
        "pg_dsn" => Ok(ParsedSecret::PgDsn(parse_pg_dsn(
            table,
            &name,
            &secret_ref,
        )?)),
        "hmac_sign" => Ok(ParsedSecret::Hmac(parse_hmac(table, &name)?)),
        "brokered_token" => Ok(ParsedSecret::BrokerToken(parse_broker_token(
            table,
            &name,
            default_hosts,
        )?)),
        "aws_auth" => Ok(ParsedSecret::AwsAuth(parse_aws(table, &name)?)),
        other => bail!("unknown secret type {other:?} for secret {name:?}"),
    }
}

fn parse_http(
    table: &toml::Table,
    name: &str,
    secret_ref: &str,
    default_hosts: &[String],
) -> Result<HttpSecret> {
    let mode = match table
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("replace")
    {
        "replace" => SecretMode::Replace,
        "inject" => SecretMode::Inject,
        other => bail!(
            "HTTP secret {name:?} has unknown mode {other:?} (expected 'replace' or 'inject')"
        ),
    };
    let hosts = match non_empty_str_array(table.get("hosts")) {
        Some(hosts) => hosts,
        None if !default_hosts.is_empty() => default_hosts.to_vec(),
        None => bail!(
            "HTTP secret {name:?} 'hosts' must be a non-empty array of non-empty strings \
             (or the tool must declare top-level hosts); a secret without hosts would be \
             unscoped in iron-proxy"
        ),
    };

    match mode {
        SecretMode::Replace => {
            reject_keys(
                table,
                name,
                "replace",
                &["inject_header", "inject_formatter", "inject_query_param"],
            )?;
            let match_headers = str_array(table.get("match_headers")).unwrap_or_default();
            let match_path = bool_field(table, name, "match_path")?;
            let match_query = bool_field(table, name, "match_query")?;
            if match_headers.is_empty() && !match_path && !match_query {
                bail!(
                    "replace-mode HTTP secret {name:?} must declare where iron-proxy \
                     scans for it: 'match_headers', 'match_path', and/or 'match_query'"
                );
            }
            let replacer = table
                .get("replacer")
                .map(|v| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned))
                .unwrap_or_else(|| Some(name.to_owned()))
                .ok_or_else(|| eyre!("HTTP secret {name:?} has invalid 'replacer'"))?;
            Ok(HttpSecret {
                name: name.to_owned(),
                secret_ref: secret_ref.to_owned(),
                mode,
                hosts,
                replacer,
                match_headers,
                match_path,
                match_query,
                inject_header: None,
                inject_formatter: None,
                inject_query_param: None,
            })
        }
        SecretMode::Inject => {
            reject_keys(
                table,
                name,
                "inject",
                &["replacer", "match_headers", "match_path", "match_query"],
            )?;
            let inject_header = opt_str(table, "inject_header");
            let inject_query_param = opt_str(table, "inject_query_param");
            let inject_formatter = opt_str(table, "inject_formatter");
            if inject_header.is_some() == inject_query_param.is_some() {
                bail!(
                    "inject-mode HTTP secret {name:?} must declare exactly one of \
                     'inject_header' or 'inject_query_param'"
                );
            }
            if inject_formatter.is_some() && inject_header.is_none() {
                bail!(
                    "inject-mode HTTP secret {name:?} sets 'inject_formatter', which \
                     only applies alongside 'inject_header'"
                );
            }
            Ok(HttpSecret {
                name: name.to_owned(),
                secret_ref: secret_ref.to_owned(),
                mode,
                hosts,
                replacer: String::new(),
                match_headers: vec![],
                match_path: false,
                match_query: false,
                inject_header,
                inject_formatter,
                inject_query_param,
            })
        }
    }
}

fn parse_oauth(table: &toml::Table, name: &str) -> Result<OAuthTokenSecret> {
    let grant = req_str(table, "grant").wrap_err_with(|| format!("oauth_token entry {name:?}"))?;
    let (_, required, optional) = OAUTH_GRANT_FIELDS
        .iter()
        .find(|(g, _, _)| *g == grant)
        .ok_or_else(|| {
            let grants: Vec<&str> = OAUTH_GRANT_FIELDS.iter().map(|(g, _, _)| *g).collect();
            eyre!("oauth_token entry {name:?} 'grant' must be one of {grants:?}, got {grant:?}")
        })?;

    let hosts = non_empty_str_array(table.get("hosts")).ok_or_else(|| {
        eyre!("oauth_token entry {name:?} 'hosts' must be a non-empty array of non-empty strings")
    })?;
    let scopes = str_array(table.get("scopes")).unwrap_or_default();
    let token_endpoint = match table.get("token_endpoint") {
        Some(v) => Some(
            v.as_str()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    eyre!("oauth_token entry {name:?} 'token_endpoint' must be a non-empty string")
                })?
                .to_owned(),
        ),
        None => None,
    };

    let fields = parse_field_map(table.get("fields"), name, "fields")?;
    if fields.is_empty() {
        bail!("oauth_token entry {name:?} 'fields' must be a non-empty table");
    }
    for (field, _) in &fields {
        if !required.contains(&field.as_str()) && !optional.contains(&field.as_str()) {
            bail!("oauth_token entry {name:?} field {field:?} is not valid for grant {grant:?}");
        }
    }
    for req in *required {
        if !fields.iter().any(|(f, _)| f == req) {
            bail!("oauth_token entry {name:?} grant {grant:?} requires field {req:?}");
        }
    }

    let token_endpoint_headers = match table.get("token_endpoint_headers") {
        Some(v) => parse_field_map(Some(v), name, "token_endpoint_headers")?,
        None => vec![],
    };

    let audience = opt_str(table, "audience");
    if grant == "jwt_bearer" {
        if audience.is_none() {
            bail!("oauth_token entry {name:?} grant 'jwt_bearer' requires a non-empty 'audience'");
        }
    } else if audience.is_some() {
        bail!("oauth_token entry {name:?} 'audience' is only valid for grant 'jwt_bearer'");
    }

    Ok(OAuthTokenSecret {
        name: name.to_owned(),
        grant,
        hosts,
        fields,
        scopes,
        token_endpoint,
        token_endpoint_headers,
        audience,
    })
}

fn parse_gcp(table: &toml::Table, name: &str, secret_ref: &str) -> Result<GcpAuthSecret> {
    let hosts = non_empty_str_array(table.get("hosts")).ok_or_else(|| {
        eyre!("gcp_auth entry {name:?} 'hosts' must be a non-empty array of non-empty strings")
    })?;
    let scopes = str_array(table.get("scopes")).unwrap_or_default();
    Ok(GcpAuthSecret {
        name: name.to_owned(),
        secret_ref: secret_ref.to_owned(),
        hosts,
        scopes,
    })
}

fn parse_gcp_id_token(
    table: &toml::Table,
    name: &str,
    secret_ref: &str,
) -> Result<GcpIdTokenSecret> {
    let hosts = non_empty_str_array(table.get("hosts")).ok_or_else(|| {
        eyre!("gcp_id_token entry {name:?} 'hosts' must be a non-empty array of non-empty strings")
    })?;
    let audience = req_str(table, "audience")
        .wrap_err_with(|| format!("gcp_id_token entry {name:?} requires a non-empty 'audience'"))?;
    let header = opt_str(table, "header")
        .map(validate_gcp_id_token_header)
        .transpose()
        .wrap_err_with(|| format!("gcp_id_token entry {name:?}"))?;
    Ok(GcpIdTokenSecret {
        name: name.to_owned(),
        secret_ref: secret_ref.to_owned(),
        hosts,
        audience,
        header,
    })
}

fn parse_pg_dsn(table: &toml::Table, name: &str, secret_ref: &str) -> Result<PgDsnSecret> {
    let database = req_str(table, "database")
        .wrap_err_with(|| format!("pg_dsn entry {name:?} requires a non-empty 'database'"))?;
    let role = opt_str(table, "role");
    let settings = parse_pg_dsn_settings(table.get("settings"))?;
    Ok(PgDsnSecret {
        name: name.to_owned(),
        secret_ref: secret_ref.to_owned(),
        database,
        role,
        settings,
    })
}

fn parse_pg_dsn_settings(value: Option<&Value>) -> Result<Vec<PgDsnSetting>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| eyre!("pg_dsn 'settings' must be an array"))?;
    array
        .iter()
        .map(|item| {
            let table = item
                .as_table()
                .ok_or_else(|| eyre!("pg_dsn setting must be a table"))?;
            let name = req_str(table, "name").wrap_err("pg_dsn setting")?;
            let literal = opt_str(table, "value");
            let value_from = parse_pg_dsn_setting_value_from(table.get("value_from"))?;
            if literal.is_some() == value_from.is_some() {
                bail!("pg_dsn setting {name:?} must declare exactly one of value or value_from");
            }
            Ok(PgDsnSetting {
                name,
                value: literal,
                value_from,
            })
        })
        .collect()
}

fn parse_pg_dsn_setting_value_from(value: Option<&Value>) -> Result<Option<PgDsnSettingValueFrom>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let table = value
        .as_table()
        .ok_or_else(|| eyre!("pg_dsn setting value_from must be a table"))?;
    let principal_label = opt_str(table, "principal_label");
    let principal_field = opt_str(table, "principal_field");
    if principal_label.is_none() && principal_field.is_none() {
        bail!("pg_dsn setting value_from must declare principal_label or principal_field");
    }
    Ok(Some(PgDsnSettingValueFrom {
        principal_label,
        principal_field,
    }))
}

/// Port of the `hmac_sign` branch of `_parse_secret`. `hosts` is required (no
/// tool-level fallback, matching the Python loader), `credentials` must include
/// the `secret` HMAC key, and the encoding/algorithm/timestamp fields are
/// validated against the same enums iron-proxy accepts.
fn parse_hmac(table: &toml::Table, name: &str) -> Result<HmacSignSecret> {
    let hosts = non_empty_str_array(table.get("hosts")).ok_or_else(|| {
        eyre!("hmac_sign entry {name:?} 'hosts' must be a non-empty array of non-empty strings")
    })?;
    let credentials = parse_hmac_credentials(table.get("credentials"), name)?;
    let headers = parse_hmac_headers(table.get("headers"), name)?;
    let algorithm = parse_hmac_enum(table, name, "algorithm", HMAC_ALGORITHMS)?;
    let key_encoding = parse_hmac_enum(table, name, "key_encoding", HMAC_KEY_ENCODINGS)?;
    let output_encoding = parse_hmac_enum(table, name, "output_encoding", HMAC_OUTPUT_ENCODINGS)?;
    let timestamp_format =
        parse_hmac_enum(table, name, "timestamp_format", HMAC_TIMESTAMP_FORMATS)?;
    let message = req_str(table, "message").wrap_err_with(|| {
        format!("hmac_sign entry {name:?} 'message' must be a non-empty Go-template string")
    })?;
    let allow_chunked_body = bool_field_named("hmac_sign", table, name, "allow_chunked_body")?;
    Ok(HmacSignSecret {
        name: name.to_owned(),
        hosts,
        credentials,
        headers,
        algorithm,
        key_encoding,
        output_encoding,
        message,
        timestamp_format,
        allow_chunked_body,
    })
}

/// Port of the `aws_auth` branch of `_parse_secret`. `hosts` is required (no
/// tool-level fallback, matching the Python loader), `access_key_id` and
/// `secret_access_key` are required non-empty credential refs, `session_token`
/// is an optional ref, and `allowed_regions`/`allowed_services` are optional
/// scoping arrays of non-empty strings.
fn parse_aws(table: &toml::Table, name: &str) -> Result<AwsAuthSecret> {
    let hosts = non_empty_str_array(table.get("hosts")).ok_or_else(|| {
        eyre!("aws_auth entry {name:?} 'hosts' must be a non-empty array of non-empty strings")
    })?;
    let access_key_id_ref = opt_str(table, "access_key_id")
        .ok_or_else(|| eyre!("aws_auth entry {name:?} requires a non-empty 'access_key_id'"))?;
    let secret_access_key_ref = opt_str(table, "secret_access_key")
        .ok_or_else(|| eyre!("aws_auth entry {name:?} requires a non-empty 'secret_access_key'"))?;
    let session_token_ref = match table.get("session_token") {
        None => None,
        Some(_) => Some(opt_str(table, "session_token").ok_or_else(|| {
            eyre!("aws_auth entry {name:?} 'session_token' must be a non-empty string")
        })?),
    };
    let allowed_regions = aws_str_array(table.get("allowed_regions"), name, "allowed_regions")?;
    let allowed_services = aws_str_array(table.get("allowed_services"), name, "allowed_services")?;
    Ok(AwsAuthSecret {
        name: name.to_owned(),
        hosts,
        access_key_id_ref,
        secret_access_key_ref,
        session_token_ref,
        allowed_regions,
        allowed_services,
    })
}

/// An optional `aws_auth` scoping array: absent yields `[]`, present must be an
/// array of non-empty strings.
fn aws_str_array(value: Option<&Value>, name: &str, key: &str) -> Result<Vec<String>> {
    let Some(value) = value else {
        return Ok(vec![]);
    };
    let arr = value.as_array().ok_or_else(|| {
        eyre!("aws_auth entry {name:?} {key:?} must be an array of non-empty strings")
    })?;
    arr.iter()
        .map(|item| {
            item.as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    eyre!("aws_auth entry {name:?} {key:?} must be an array of non-empty strings")
                })
        })
        .collect()
}

/// The default injection for a `brokered_token` secret: the broker's current
/// access token written as a Bearer `Authorization` header. Mirrors the Python
/// loader's `_build_token_broker_entries`.
const BROKER_TOKEN_DEFAULT_HEADER: &str = "Authorization";
const BROKER_TOKEN_DEFAULT_FORMATTER: &str = "Bearer {{.Value}}";

/// Parse a `type = "brokered_token"` entry. Only the consumer side is modeled:
/// the broker credential referenced by `credential` (default `name`) is created
/// out of band via `centaur-perms broker create`. Legacy keys (`fields`,
/// `token_endpoint`, `scopes`) that described the broker credential are ignored.
fn parse_broker_token(
    table: &toml::Table,
    name: &str,
    default_hosts: &[String],
) -> Result<BrokerTokenSecret> {
    let hosts = match non_empty_str_array(table.get("hosts")) {
        Some(hosts) => hosts,
        None if !default_hosts.is_empty() => default_hosts.to_vec(),
        None => bail!(
            "brokered_token entry {name:?} 'hosts' must be a non-empty array of non-empty \
             strings (or the tool must declare top-level hosts)"
        ),
    };
    let credential = opt_str(table, "credential").unwrap_or_else(|| name.to_owned());
    let inject_header =
        opt_str(table, "inject_header").unwrap_or_else(|| BROKER_TOKEN_DEFAULT_HEADER.to_owned());
    let inject_formatter = opt_str(table, "inject_formatter")
        .unwrap_or_else(|| BROKER_TOKEN_DEFAULT_FORMATTER.to_owned());
    Ok(BrokerTokenSecret {
        name: name.to_owned(),
        credential,
        hosts,
        inject_header,
        inject_formatter,
    })
}

/// Parse one `secret_ref | {secret_ref, json_key}` value into a [`FieldSource`].
/// `ctx` is the error prefix identifying the entry and field, e.g.
/// `oauth_token entry "x" field "y"`.
fn parse_field_source(raw: &Value, ctx: &str) -> Result<FieldSource> {
    if let Some(s) = raw.as_str() {
        if s.is_empty() {
            bail!("{ctx} 'secret_ref' must be non-empty");
        }
        Ok(FieldSource {
            secret_ref: s.to_owned(),
            json_key: None,
        })
    } else if let Some(t) = raw.as_table() {
        let ref_ = req_str(t, "secret_ref").wrap_err_with(|| ctx.to_owned())?;
        let json_key = match t.get("json_key") {
            Some(v) => Some(
                v.as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| eyre!("{ctx} 'json_key' must be a non-empty string"))?
                    .to_owned(),
            ),
            None => None,
        };
        Ok(FieldSource {
            secret_ref: ref_,
            json_key,
        })
    } else {
        bail!("{ctx} must be a string or table");
    }
}

/// Parse the `credentials` table for an `hmac_sign` entry, requiring the
/// `secret` HMAC key. Mirrors `_parse_hmac_credentials`.
fn parse_hmac_credentials(value: Option<&Value>, name: &str) -> Result<Vec<(String, FieldSource)>> {
    let table = value
        .and_then(Value::as_table)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| eyre!("hmac_sign entry {name:?} 'credentials' must be a non-empty table"))?;
    let mut out = Vec::with_capacity(table.len());
    for (field, raw) in table {
        let src = parse_field_source(
            raw,
            &format!("hmac_sign entry {name:?} credential {field:?}"),
        )?;
        out.push((field.clone(), src));
    }
    if !out
        .iter()
        .any(|(field, _)| field == HMAC_REQUIRED_CREDENTIAL)
    {
        bail!(
            "hmac_sign entry {name:?} 'credentials' must include {HMAC_REQUIRED_CREDENTIAL:?} (the HMAC key)"
        );
    }
    Ok(out)
}

/// Parse the ordered `headers` list iron-proxy writes onto the request.
/// Mirrors `_parse_hmac_headers`.
fn parse_hmac_headers(value: Option<&Value>, name: &str) -> Result<Vec<HmacHeader>> {
    let arr = value
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| eyre!("hmac_sign entry {name:?} 'headers' must be a non-empty list"))?;
    let mut headers = Vec::with_capacity(arr.len());
    for (index, entry) in arr.iter().enumerate() {
        let table = entry
            .as_table()
            .ok_or_else(|| eyre!("hmac_sign entry {name:?} header[{index}] must be a table"))?;
        let header_name = req_str(table, "name").wrap_err_with(|| {
            format!("hmac_sign entry {name:?} header[{index}] requires a non-empty 'name'")
        })?;
        let value = req_str(table, "value").wrap_err_with(|| {
            format!(
                "hmac_sign entry {name:?} header[{index}] requires a non-empty 'value' template"
            )
        })?;
        headers.push(HmacHeader {
            name: header_name,
            value,
        });
    }
    Ok(headers)
}

/// Validate one `hmac_sign` enum field against `allowed`. Mirrors
/// `_parse_hmac_enum`.
fn parse_hmac_enum(table: &toml::Table, name: &str, key: &str, allowed: &[&str]) -> Result<String> {
    match table.get(key).and_then(Value::as_str) {
        Some(value) if allowed.contains(&value) => Ok(value.to_owned()),
        other => {
            bail!("hmac_sign entry {name:?} {key:?} must be one of {allowed:?}, got {other:?}")
        }
    }
}

/// Parse a `{field = secret_ref | {secret_ref, json_key}}` table into ordered
/// `(field, FieldSource)` pairs. Mirrors `_parse_oauth_field_source`.
fn parse_field_map(
    value: Option<&Value>,
    secret: &str,
    what: &str,
) -> Result<Vec<(String, FieldSource)>> {
    let Some(value) = value else {
        return Ok(vec![]);
    };
    let table = value
        .as_table()
        .ok_or_else(|| eyre!("oauth_token entry {secret:?} {what:?} must be a table"))?;
    let mut out = Vec::with_capacity(table.len());
    for (field, raw) in table {
        let src = parse_field_source(
            raw,
            &format!("oauth_token entry {secret:?} field {field:?}"),
        )?;
        out.push((field.clone(), src));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// small toml helpers
// ---------------------------------------------------------------------------

fn req_str(table: &toml::Table, key: &str) -> Result<String> {
    table
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| eyre!("missing non-empty {key:?}"))
}

fn opt_str(table: &toml::Table, key: &str) -> Option<String> {
    table
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn bool_field(table: &toml::Table, name: &str, key: &str) -> Result<bool> {
    bool_field_named("HTTP secret", table, name, key)
}

/// Like [`bool_field`] but labels the error with `kind` (e.g. `"hmac_sign"`).
fn bool_field_named(kind: &str, table: &toml::Table, name: &str, key: &str) -> Result<bool> {
    match table.get(key) {
        None => Ok(false),
        Some(Value::Boolean(b)) => Ok(*b),
        Some(_) => bail!("{kind} entry {name:?} has invalid {key:?} (expected a boolean)"),
    }
}

/// An array of strings, or `None` if the key is absent. Non-string members are
/// dropped (the API logs and skips them too); an empty/missing key yields `None`.
fn str_array(value: Option<&Value>) -> Option<Vec<String>> {
    let arr = value?.as_array()?;
    Some(
        arr.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    )
}

/// A non-empty array of non-empty strings, or `None` if absent/invalid.
fn non_empty_str_array(value: Option<&Value>) -> Option<Vec<String>> {
    let arr = value?.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item.as_str().filter(|s| !s.is_empty())?;
        out.push(s.to_owned());
    }
    Some(out)
}

fn validate_gcp_id_token_header(value: String) -> Result<String> {
    normalize_gcp_id_token_header(&value).ok_or_else(|| {
        eyre!(
            "header must be one of {}, got {value:?}",
            GCP_ID_TOKEN_ALLOWED_HEADERS.join(", ")
        )
    })
}

fn reject_keys(table: &toml::Table, name: &str, mode: &str, keys: &[&str]) -> Result<()> {
    for key in keys {
        if table.contains_key(*key) {
            bail!("{mode}-mode HTTP secret {name:?} must not declare {key:?}");
        }
    }
    Ok(())
}
