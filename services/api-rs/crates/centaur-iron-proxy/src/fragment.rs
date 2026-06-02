use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::{IronProxyConfigError, ProxyFragment, Result};

pub const DEFAULT_PROXY_BASE_CONFIG: &str =
    include_str!("../../../../api/api/iron-proxy.base.yaml");
pub const INFRA_FRAGMENT: &str = include_str!("../../../../iron-proxy/infra.yaml");
pub const CLAUDE_CODE_API_KEY_FRAGMENT: &str =
    include_str!("../../../../iron-proxy/harness/claude-code-api-key.yaml");
pub const CLAUDE_CODE_ACCESS_TOKEN_FRAGMENT: &str =
    include_str!("../../../../iron-proxy/harness/claude-code-access-token.yaml");
pub const CODEX_API_KEY_FRAGMENT: &str =
    include_str!("../../../../iron-proxy/harness/codex-api-key.yaml");
pub const CODEX_ACCESS_TOKEN_FRAGMENT: &str =
    include_str!("../../../../iron-proxy/harness/codex-access-token.yaml");

pub fn load_fragment_file(path: impl AsRef<Path>) -> Result<ProxyFragment> {
    let path = path.as_ref();
    let contents = fs::read_to_string(path).map_err(|source| IronProxyConfigError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
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
        } else if file_type.is_file()
            && path.file_name().and_then(|name| name.to_str()) == Some("iron.yaml")
        {
            paths.push(path);
        }
    }
    Ok(())
}

pub fn harness_fragment(engine: &str, auth_mode: &str) -> Result<Option<ProxyFragment>> {
    let contents = match (engine, auth_mode) {
        ("claude-code", "access_token") => CLAUDE_CODE_ACCESS_TOKEN_FRAGMENT,
        ("claude-code", _) => CLAUDE_CODE_API_KEY_FRAGMENT,
        ("codex", "access_token") => CODEX_ACCESS_TOKEN_FRAGMENT,
        ("codex", _) => CODEX_API_KEY_FRAGMENT,
        _ => return Ok(None),
    };
    load_fragment_str(contents).map(Some)
}

pub fn infra_fragment() -> Result<ProxyFragment> {
    load_fragment_str(INFRA_FRAGMENT)
}

pub fn harness_broker_fragments() -> Result<Vec<ProxyFragment>> {
    [
        CLAUDE_CODE_ACCESS_TOKEN_FRAGMENT,
        CODEX_ACCESS_TOKEN_FRAGMENT,
    ]
    .into_iter()
    .map(load_fragment_str)
    .collect()
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
