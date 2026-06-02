use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use crate::{IronProxyConfigError, ProxyFragment, Result};

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
        } else if file_type.is_file()
            && path.file_name().and_then(|name| name.to_str()) == Some("iron.yaml")
        {
            paths.push(path);
        }
    }
    Ok(())
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
