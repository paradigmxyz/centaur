//! Tool sources for agent sandboxes.
//!
//! api-rs serves no `/tools` HTTP registry. Instead the agent image installs
//! each tool as a shell CLI shim at entrypoint
//! (`services/sandbox/install_tool_shims.py`) by scanning `TOOL_DIRS` for
//! `pyproject.toml [project.scripts]` and `uvx`-installing each. Secrets ride
//! proxied env (tool placeholder creds + `*_DSN` from `apply_proxy_env`,
//! granted per-sandbox by iron-control) — none of that lives here.
//!
//! What this module provides is the *sources* the shims install from — the same
//! trees api-rs's own `tool_discovery` scans, so the creds api-rs grants match
//! the tools the agent installs:
//!
//! * a `tools-bootstrap` init container publishes the tools repo's `source_subdir`
//!   from the repo-cache DaemonSet's node-level cache when configured, otherwise
//!   it git-clones the tools repo at a pinned ref into an emptyDir mounted at
//!   `/app/tools`;
//!
//! `TOOL_DIRS` is set explicitly on the agent env to `/app/tools`, pointing at
//! the path the init container populates in this pod. The published tools tree
//! keeps source metadata in a hidden file so `centaur-tools refresh` can either
//! republish from repo-cache or fetch the configured ref, then reinstall shims
//! without restarting the pod.

use centaur_sandbox_core::RepoCacheAccess;
use serde_json::{Value, json};

const AGENT_UID: i64 = 1001;

/// Base tools path inside both the api-rs pod and the agent sandbox.
pub(crate) const BASE_TOOL_DIR: &str = "/app/tools";
/// Base Centaur tools baked into the sandbox image. Restricted sandboxes use
/// this path so they keep first-party tool shims without mounting repo-cache or
/// publishing overlay sources.
pub(crate) const BAKED_BASE_TOOL_DIR: &str = "/opt/centaur/tools";
/// emptyDir the `tools-bootstrap` init container publishes the tools tree into.
const TOOLS_VOLUME: &str = "tools-root";
/// Staging path where `tools-bootstrap` mounts the tools emptyDir. The agent
/// container mounts the same volume read-only at `BASE_TOOL_DIR`.
const TOOLS_BOOTSTRAP_DIR: &str = "/tools-bootstrap";
/// Volume + mount carrying the GitHub token for private-repo clones (askpass).
const GITHUB_TOKEN_VOLUME: &str = "tools-github-token";
const GITHUB_TOKEN_DIR: &str = "/tools-github-token";
const GITHUB_TOKEN_FILE: &str = "token";
const GITHUB_TOKEN_FILE_PATH: &str = "/tools-github-token/token";
const REPO_CACHE_VOLUME: &str = "tools-repo-cache";
const PUBLIC_VISIBILITY: &str = "public";
const PRIVATE_VISIBILITY: &str = "private";

/// Git source for the base tools tree. When set, every sandbox gets a
/// `tools-bootstrap` init container that clones `repo` at `git_ref` and
/// publishes its `source_subdir` into the agent's `/app/tools` — so adding a
/// tool is a push to the repo, not an image rebuild.
#[derive(Clone, Debug)]
pub struct ToolsConfig {
    /// `owner/name` GitHub repo carrying the tools tree.
    pub repo: String,
    /// Branch, tag, or commit to check out. `None` => the repo's default branch.
    pub git_ref: Option<String>,
    /// Subdirectory within the repo holding the tools (published to `/app/tools`).
    pub source_subdir: String,
    /// Repo visibility. Public sandboxes only publish public sources.
    pub visibility: String,
    /// Image the clone init container runs. It must include git and
    /// `install-tool-shims` (the default sandbox image does).
    pub image: String,
    pub image_pull_policy: Option<String>,
    /// GitHub token secret for private-repo clones. `None` => unauthenticated clone.
    pub github_token: Option<GitHubTokenRef>,
    /// Optional repo-cache root path mounted into the sandbox. When set,
    /// tools-bootstrap publishes from `<repo_cache_path>/<repo>/<source_subdir>`
    /// and `centaur-tools refresh` republishes from the same cache instead of
    /// fetching. By default this path is mounted from the host; set
    /// `repo_cache_pvc` to mount the path from a PersistentVolumeClaim instead.
    pub repo_cache_path: Option<String>,
    /// Optional PVC backing for `repo_cache_path`. This lets Autopilot clusters use
    /// repoCache without hostPath volumes.
    pub repo_cache_pvc: Option<String>,
    /// Optional subdirectory within the repo-cache volume to expose at
    /// `repo_cache_path`. Used for public-only repo-cache access.
    pub repo_cache_sub_path: Option<String>,
    /// Whether running sandboxes should watch repo-cache checkouts and refresh
    /// local tool shims when commits change.
    pub auto_reload: bool,
    /// Additional tool sources copied after the base tree. Duplicate tool names
    /// are skipped by the copy helper.
    pub extra_sources: Vec<ToolSource>,
}

/// One repo/subdir tools source copied into `/app/tools`.
#[derive(Clone, Debug)]
pub struct ToolSource {
    pub repo: String,
    pub git_ref: Option<String>,
    pub source_subdir: String,
    pub visibility: String,
}

impl ToolSource {
    fn is_public(&self) -> bool {
        self.visibility == PUBLIC_VISIBILITY
    }
}

/// A Kubernetes Secret key holding a GitHub token, fed to `git` via `GIT_ASKPASS`.
#[derive(Clone, Debug)]
pub struct GitHubTokenRef {
    pub secret_name: String,
    pub secret_key: String,
}

impl ToolsConfig {
    pub fn new(repo: impl Into<String>, image: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            git_ref: None,
            source_subdir: "tools".to_owned(),
            visibility: PRIVATE_VISIBILITY.to_owned(),
            image: image.into(),
            image_pull_policy: None,
            github_token: None,
            repo_cache_path: None,
            repo_cache_pvc: None,
            repo_cache_sub_path: None,
            auto_reload: true,
            extra_sources: Vec::new(),
        }
    }

    fn sources(&self) -> Vec<ToolSource> {
        let mut sources = vec![ToolSource {
            repo: self.repo.clone(),
            git_ref: self.git_ref.clone(),
            source_subdir: self.source_subdir.clone(),
            visibility: self.visibility.clone(),
        }];
        sources.extend(self.extra_sources.clone());
        sources
    }

    pub(crate) fn has_sources(&self) -> bool {
        !self.repo.is_empty()
    }

    pub fn scoped_for_repo_cache_access(&self, access: &RepoCacheAccess) -> Self {
        let mut scoped = self.clone();
        scoped.repo_cache_sub_path = match access {
            RepoCacheAccess::Public => Some(PUBLIC_VISIBILITY.to_owned()),
            RepoCacheAccess::None | RepoCacheAccess::All => None,
        };
        if matches!(access, RepoCacheAccess::Public) {
            scoped.extra_sources.retain(ToolSource::is_public);
            if scoped.visibility != PUBLIC_VISIBILITY {
                if let Some(first_public) = scoped.extra_sources.first().cloned() {
                    scoped.repo = first_public.repo;
                    scoped.git_ref = first_public.git_ref;
                    scoped.source_subdir = first_public.source_subdir;
                    scoped.visibility = first_public.visibility;
                    scoped.extra_sources.remove(0);
                } else {
                    scoped.repo.clear();
                }
            }
        }
        scoped
    }

    fn repo_cache_source_path(&self) -> Option<String> {
        let repo_cache_path = self.repo_cache_path.as_ref()?;
        if self.repo_cache_pvc.is_some() {
            return Some(repo_cache_path.clone());
        }
        Some(match &self.repo_cache_sub_path {
            Some(sub_path) => format!(
                "{}/{}",
                repo_cache_path.trim_end_matches('/'),
                sub_path.trim_start_matches('/')
            ),
            None => repo_cache_path.clone(),
        })
    }

    fn repo_cache_volume_mount(&self) -> Option<Value> {
        let repo_cache_path = self.repo_cache_path.as_ref()?;
        let mut mount = json!({
            "name": REPO_CACHE_VOLUME,
            "mountPath": repo_cache_path,
            "readOnly": true,
        });
        if self.repo_cache_pvc.is_some()
            && let Some(sub_path) = &self.repo_cache_sub_path
            && let Some(obj) = mount.as_object_mut()
        {
            obj.insert("subPath".to_owned(), json!(sub_path));
        }
        Some(mount)
    }
}

pub(crate) fn security_context_json() -> Value {
    json!({
        "allowPrivilegeEscalation": false,
        "capabilities": {"drop": ["ALL"]},
        "runAsGroup": AGENT_UID,
        "runAsNonRoot": true,
        "runAsUser": AGENT_UID,
        "seccompProfile": {"type": "RuntimeDefault"},
    })
}

pub(crate) fn pod_security_context_json() -> Value {
    json!({
        "fsGroup": AGENT_UID,
        "fsGroupChangePolicy": "OnRootMismatch",
    })
}

/// `TOOL_DIRS` for the agent. Matches the path tools-bootstrap populates.
pub(crate) fn agent_tool_dirs() -> String {
    BASE_TOOL_DIR.to_owned()
}

/// `TOOL_DIRS` for restricted sandboxes that should not receive repo-cache or
/// overlay-derived tool sources.
pub(crate) fn baked_base_tool_dirs() -> String {
    BAKED_BASE_TOOL_DIR.to_owned()
}

/// Agent env added for tools wiring.
pub(crate) fn agent_env(tools: Option<&ToolsConfig>) -> Vec<(String, String)> {
    let mut env = vec![("TOOL_DIRS".to_owned(), agent_tool_dirs())];
    if let Some(tools) = tools {
        env.push((
            "CENTAUR_TOOLS_AUTO_RELOAD".to_owned(),
            tools.auto_reload.to_string(),
        ));
    }
    if tools
        .and_then(|tools| tools.github_token.as_ref())
        .is_some()
    {
        env.push((
            "CENTAUR_TOOLS_GITHUB_TOKEN_FILE".to_owned(),
            GITHUB_TOKEN_FILE_PATH.to_owned(),
        ));
    }
    env
}

/// Agent env for baked base tools only. No GitHub token is exposed because
/// restricted sandboxes do not refresh from git or repo-cache.
pub(crate) fn baked_base_agent_env() -> Vec<(String, String)> {
    vec![("TOOL_DIRS".to_owned(), baked_base_tool_dirs())]
}

/// Routes the tools clone through the per-sandbox egress proxy. The sandbox
/// NetworkPolicy only allows egress to the proxy, api-rs, and DNS — a direct
/// clone to github.com is blocked whenever iron-proxy is enabled. The proxy
/// MITMs TLS (github.com is in the baseline allowlist), so git must trust the
/// firewall CA it re-signs with.
pub(crate) struct CloneProxy {
    /// Per-sandbox proxy URL (the `HTTPS_PROXY` value `apply_proxy_env` set).
    pub https_proxy: String,
    /// Path to the firewall CA cert inside the container.
    pub ca_cert_path: String,
    /// Mount of the pod's existing `firewall-ca` volume for the init container.
    pub ca_volume_mount: Value,
}

/// The `tools-bootstrap` init container: resolves each configured source, then
/// delegates copying to `install-tool-shims`. With a `CloneProxy`, the clone
/// fallback rides the per-sandbox iron-proxy like all other sandbox egress.
pub(crate) fn tools_init_container_json(
    tools: &ToolsConfig,
    clone_proxy: Option<&CloneProxy>,
) -> Value {
    let sources = tools.sources();

    let proxy_exports = match clone_proxy {
        Some(proxy) => format!(
            "export HTTPS_PROXY=\"{https_proxy}\"\n\
             export https_proxy=\"{https_proxy}\"\n\
             export GIT_SSL_CAINFO=\"{ca_cert_path}\"\n",
            https_proxy = proxy.https_proxy,
            ca_cert_path = proxy.ca_cert_path,
        ),
        None => String::new(),
    };

    // GIT_ASKPASS feeds the token as the HTTPS password (user x-access-token),
    // matching the repo-cache DaemonSet. Wired only when a token secret is mounted.
    let askpass = if tools.github_token.is_some() {
        format!(
            "printf '#!/bin/sh\\ncase \"$1\" in *Username*) echo x-access-token;; \
             *Password*) cat {GITHUB_TOKEN_DIR}/{GITHUB_TOKEN_FILE};; *) echo;; esac\\n' \
             > /tmp/git-askpass\n\
             chmod 0700 /tmp/git-askpass\n\
             export GIT_ASKPASS=/tmp/git-askpass\n"
        )
    } else {
        String::new()
    };

    // The per-sandbox proxy is created in the same reconcile as the Sandbox and
    // may not be accepting connections when this init container first runs — and
    // an init failure is terminal for the Sandbox (no kubelet retry), so the
    // clone must retry through the connection-refused window rather than die.
    // repo/ref/subdir are operator config, but quote them anyway so a stray
    // space or metacharacter breaks loudly in git instead of in the shell.
    let mut publish_steps = String::new();
    let mut metadata_sources = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let subdir = &source.source_subdir;
        let repo = &source.repo;
        if let Some(repo_cache_path) = &tools.repo_cache_path {
            let repo_cache_repo_path = format!(
                "{}/{}",
                repo_cache_path.trim_end_matches('/'),
                source.repo.trim_start_matches('/')
            );
            // Wait only for the repo checkout itself; a source without the
            // tools subdir is skipped instead of failing the sandbox init.
            publish_steps.push_str(&format!(
                "attempt=0\n\
                 cache_repo=\"{repo_cache_repo_path}\"\n\
                 until [ -d \"$cache_repo/.git\" ]; do\n\
                 attempt=$((attempt + 1))\n\
                 if [ \"$attempt\" -ge 30 ]; then echo \"tools repo-cache entry unavailable after $attempt attempts: $cache_repo\" >&2; exit 1; fi\n\
                 sleep 2\n\
                 done\n\
                 if [ -d \"$cache_repo/{subdir}\" ]; then\n\
                 install-tool-shims --copy-tools \"$cache_repo/{subdir}\" \"$target\"\n\
                 else\n\
                 echo \"skipping tools source {repo}: no {subdir}/ in repo-cache checkout\" >&2\n\
                 fi\n"
            ));
            metadata_sources.push(json!({
                "repo": source.repo,
                "source_subdir": subdir,
                "git_ref": source.git_ref.as_deref(),
                "source": "repo_cache",
                "repo_cache_repo_path": repo_cache_repo_path,
            }));
        } else {
            let repo_url = format!("https://github.com/{}.git", source.repo);
            let source_path = if index == 0 {
                ".centaur-source".to_owned()
            } else {
                format!(".centaur-source-{index}")
            };
            let source_target_path = format!("$target/{source_path}");
            let checkout = match &source.git_ref {
                Some(git_ref) => format!(
                    "git -C \"$source\" -c gc.auto=0 fetch --quiet origin \"{git_ref}\" && \
                     git -C \"$source\" checkout --quiet --detach FETCH_HEAD"
                ),
                None => "git -C \"$source\" checkout --quiet".to_owned(),
            };
            // The clone retry guards proxy startup; a checked-out source that
            // simply lacks the subdir is skipped rather than failing the
            // sandbox (sparse-checkout of a missing path succeeds, so this is
            // only detectable after checkout).
            publish_steps.push_str(&format!(
                "source=\"{source_target_path}\"\n\
                 rm -rf \"$source\"\n\
                 attempt=0\n\
                 until git clone --quiet --filter=blob:none --no-checkout \"{repo_url}\" \"$source\" && \
                 git -C \"$source\" sparse-checkout set \"{subdir}\" && \
                 {checkout}; do\n\
                 attempt=$((attempt + 1))\n\
                 if [ \"$attempt\" -ge 30 ]; then echo \"tools clone failed after $attempt attempts\" >&2; exit 1; fi\n\
                 rm -rf \"$source\"\n\
                 sleep 2\n\
                 done\n\
                 if [ -d \"$source/{subdir}\" ]; then\n\
                 install-tool-shims --copy-tools \"$source/{subdir}\" \"$target\"\n\
                 else\n\
                 echo \"skipping tools source {repo}: no {subdir}/ at the configured ref\" >&2\n\
                 fi\n"
            ));
            metadata_sources.push(json!({
                "repo": source.repo,
                "source_subdir": subdir,
                "git_ref": source.git_ref.as_deref(),
                "source": "git",
                "source_path": source_path,
            }));
        }
    }
    let first_metadata = metadata_sources
        .first()
        .cloned()
        .unwrap_or_else(|| json!({}));
    let metadata = json!({
        "source_subdir": first_metadata.get("source_subdir").and_then(Value::as_str),
        "git_ref": first_metadata.get("git_ref").and_then(Value::as_str),
        "source": first_metadata.get("source").and_then(Value::as_str),
        "repo_cache_repo_path": first_metadata.get("repo_cache_repo_path").and_then(Value::as_str),
        "sources": metadata_sources,
    })
    .to_string();
    let script = format!(
        "set -e\n\
         {proxy_exports}\
         {askpass}\
         export GIT_TERMINAL_PROMPT=0\n\
         git config --global --add safe.directory '*'\n\
         target=\"{TOOLS_BOOTSTRAP_DIR}\"\n\
         mkdir -p \"$target\"\n\
         {publish_steps}\n\
         cat > \"$target/.centaur-tools-source.json\" <<'CENTAUR_TOOLS_METADATA'\n\
{metadata}\n\
CENTAUR_TOOLS_METADATA"
    );

    let mut volume_mounts = vec![json!({"name": TOOLS_VOLUME, "mountPath": TOOLS_BOOTSTRAP_DIR})];
    if tools.github_token.is_some() {
        volume_mounts.push(json!({
            "name": GITHUB_TOKEN_VOLUME,
            "mountPath": GITHUB_TOKEN_DIR,
            "readOnly": true,
        }));
    }
    if let Some(proxy) = clone_proxy {
        volume_mounts.push(proxy.ca_volume_mount.clone());
    }
    if let Some(mount) = tools.repo_cache_volume_mount() {
        volume_mounts.push(mount);
    }

    let mut container = json!({
        "name": "tools-bootstrap",
        "image": tools.image,
        "command": ["/bin/sh", "-ec", script],
        "volumeMounts": volume_mounts,
        "securityContext": security_context_json(),
    });
    if let Some(policy) = &tools.image_pull_policy {
        container["imagePullPolicy"] = json!(policy);
    }
    container
}

/// Volumes added to the pod for tool sources.
pub(crate) fn volumes_json(tools: Option<&ToolsConfig>) -> Vec<Value> {
    let mut volumes = Vec::new();
    if let Some(tools) = tools {
        volumes.push(json!({"name": TOOLS_VOLUME, "emptyDir": {}}));
        if let Some(token) = &tools.github_token {
            volumes.push(json!({
                "name": GITHUB_TOKEN_VOLUME,
                "secret": {
                    "secretName": token.secret_name,
                    "defaultMode": 0o400,
                    "items": [{"key": token.secret_key, "path": GITHUB_TOKEN_FILE}],
                },
            }));
        }
        if let Some(repo_cache_path) = &tools.repo_cache_path {
            if let Some(claim_name) = &tools.repo_cache_pvc {
                volumes.push(json!({
                    "name": REPO_CACHE_VOLUME,
                    "persistentVolumeClaim": {
                        "claimName": claim_name,
                        "readOnly": true,
                    },
                }));
            } else {
                volumes.push(json!({
                    "name": REPO_CACHE_VOLUME,
                    "hostPath": {
                        "path": tools.repo_cache_source_path().unwrap_or_else(|| repo_cache_path.clone()),
                        "type": "DirectoryOrCreate",
                    },
                }));
            }
        }
    }
    volumes
}

/// Volume mounts added to the AGENT container: the tools tree at `/app/tools`.
/// It is writable so `centaur-tools refresh` can publish a freshly fetched tree
/// into the same emptyDir and reinstall shims without restarting the pod.
pub(crate) fn agent_volume_mounts_json(tools: Option<&ToolsConfig>) -> Vec<Value> {
    let Some(tools) = tools else {
        return Vec::new();
    };
    let mut mounts = vec![json!({"name": TOOLS_VOLUME, "mountPath": BASE_TOOL_DIR})];
    if tools.github_token.is_some() {
        mounts.push(json!({
            "name": GITHUB_TOKEN_VOLUME,
            "mountPath": GITHUB_TOKEN_DIR,
            "readOnly": true,
        }));
    }
    if let Some(mount) = tools.repo_cache_volume_mount() {
        mounts.push(mount);
    }
    mounts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_dirs_point_at_bootstrapped_tools() {
        assert_eq!(agent_tool_dirs(), "/app/tools");
    }

    #[test]
    fn baked_base_tool_dirs_point_at_image_tools() {
        assert_eq!(baked_base_tool_dirs(), "/opt/centaur/tools");
    }

    #[test]
    fn agent_env_sets_tool_dirs() {
        let env = agent_env(None);
        assert_eq!(env, vec![("TOOL_DIRS".to_owned(), "/app/tools".to_owned())]);
    }

    #[test]
    fn agent_env_sets_auto_reload_from_tools_config() {
        let mut tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        tools.auto_reload = false;

        let env = agent_env(Some(&tools));

        assert!(env.contains(&("TOOL_DIRS".to_owned(), "/app/tools".to_owned())));
        assert!(env.contains(&("CENTAUR_TOOLS_AUTO_RELOAD".to_owned(), "false".to_owned())));
    }

    #[test]
    fn baked_base_agent_env_sets_baked_tool_dirs() {
        assert_eq!(
            baked_base_agent_env(),
            vec![("TOOL_DIRS".to_owned(), "/opt/centaur/tools".to_owned())]
        );
    }

    #[test]
    fn tools_init_clones_repo_into_emptydir() {
        let mut tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        tools.git_ref = Some("main".to_owned());
        let c = tools_init_container_json(&tools, None);
        assert_eq!(c["name"], "tools-bootstrap");
        assert_eq!(c["image"], "centaur-agent:test");
        let script = c["command"][2].as_str().unwrap();
        assert!(script.contains(
            "git clone --quiet --filter=blob:none --no-checkout \"https://github.com/paradigmxyz/centaur.git\""
        ));
        assert!(script.contains("sparse-checkout set \"tools\""));
        assert!(script.contains("fetch --quiet origin \"main\""));
        assert!(script.contains("source=\"$target/.centaur-source\""));
        assert!(script.contains("\"source_path\":\".centaur-source\""));
        assert!(script.contains("if [ -d \"$source/tools\" ]; then"));
        assert!(script.contains("install-tool-shims --copy-tools \"$source/tools\" \"$target\""));
        assert!(script.contains(
            "skipping tools source paradigmxyz/centaur: no tools/ at the configured ref"
        ));
        assert!(script.contains(".centaur-tools-source.json"));
        assert!(!script.contains("cp -R"));
        // No token configured => no askpass, single (tools) volume mount.
        assert!(!script.contains("GIT_ASKPASS"));
        assert_eq!(c["volumeMounts"].as_array().unwrap().len(), 1);
        assert_eq!(c["volumeMounts"][0]["mountPath"], "/tools-bootstrap");
    }

    #[test]
    fn tools_init_retries_clone_until_proxy_accepts() {
        // The per-sandbox proxy may not be listening when the init container
        // first runs, and an init failure is terminal for the Sandbox — the
        // clone (and the ref fetch/checkout chained into the same condition)
        // must sit in a bounded retry loop, with the copy AFTER the loop.
        let mut tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        tools.git_ref = Some("main".to_owned());
        let script = tools_init_container_json(&tools, None)["command"][2]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(script.contains("until git clone"));
        assert!(script.contains("checkout --quiet --detach FETCH_HEAD; do"));
        assert!(script.contains("if [ \"$attempt\" -ge 30 ]"));
        assert!(script.contains("sleep 2"));
        assert!(
            script.find("done").unwrap() < script.find("install-tool-shims --copy-tools").unwrap()
        );
    }

    #[test]
    fn tools_init_with_proxy_exports_proxy_env_and_mounts_ca() {
        let tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        let proxy = CloneProxy {
            https_proxy: "http://asbx-test-iron-proxy:8080".to_owned(),
            ca_cert_path: "/firewall-certs/ca-cert.pem".to_owned(),
            ca_volume_mount: json!({
                "name": "firewall-ca",
                "mountPath": "/firewall-certs",
                "readOnly": true,
            }),
        };
        let c = tools_init_container_json(&tools, Some(&proxy));
        let script = c["command"][2].as_str().unwrap();
        // Proxy exports come before the clone so git CONNECTs through iron-proxy
        // and trusts the CA it re-signs TLS with.
        assert!(script.contains("export HTTPS_PROXY=\"http://asbx-test-iron-proxy:8080\""));
        assert!(script.contains("export https_proxy=\"http://asbx-test-iron-proxy:8080\""));
        assert!(script.contains("export GIT_SSL_CAINFO=\"/firewall-certs/ca-cert.pem\""));
        assert!(script.find("export HTTPS_PROXY").unwrap() < script.find("git clone").unwrap());
        let mounts = c["volumeMounts"].as_array().unwrap();
        assert_eq!(mounts.len(), 2);
        assert!(
            mounts
                .iter()
                .any(|m| m["name"] == "firewall-ca" && m["mountPath"] == "/firewall-certs")
        );
    }

    #[test]
    fn tools_init_default_ref_checks_out_clone_head() {
        let tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        let script = tools_init_container_json(&tools, None)["command"][2]
            .as_str()
            .unwrap()
            .to_owned();
        // Default branch: plain checkout, no explicit ref fetch.
        assert!(script.contains("git -C \"$source\" checkout --quiet; do"));
        assert!(!script.contains("fetch --quiet origin"));
    }

    #[test]
    fn tools_init_can_copy_from_repo_cache() {
        let mut tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        tools.repo_cache_path = Some("/var/lib/centaur/repos".to_owned());

        let c = tools_init_container_json(&tools, None);
        let script = c["command"][2].as_str().unwrap();
        assert!(script.contains("cache_repo=\"/var/lib/centaur/repos/paradigmxyz/centaur\""));
        assert!(script.contains("if [ -d \"$cache_repo/tools\" ]; then"));
        assert!(
            script.contains("install-tool-shims --copy-tools \"$cache_repo/tools\" \"$target\"")
        );
        assert!(script.contains("\"source\":\"repo_cache\""));
        assert!(!script.contains("cp -R"));
        assert!(!script.contains("git clone"));
        // The wait covers only the repo checkout; a source without the tools
        // subdir is skipped instead of failing the sandbox, so the chart can
        // default toolsSubdir for workflows- or skills-only overlay repos.
        assert!(script.contains("until [ -d \"$cache_repo/.git\" ]; do"));
        assert!(
            !script.contains("until [ -d \"$cache_repo/.git\" ] && [ -d \"$cache_repo/tools\" ]")
        );
        assert!(script.contains(
            "skipping tools source paradigmxyz/centaur: no tools/ in repo-cache checkout"
        ));
        assert!(c["volumeMounts"].as_array().unwrap().iter().any(|mount| {
            mount["name"] == REPO_CACHE_VOLUME
                && mount["mountPath"] == "/var/lib/centaur/repos"
                && mount["readOnly"] == true
        }));

        let volumes = volumes_json(Some(&tools));
        assert!(volumes.iter().any(|volume| {
            volume["name"] == REPO_CACHE_VOLUME
                && volume["hostPath"]["path"] == "/var/lib/centaur/repos"
                && volume["hostPath"]["type"] == "DirectoryOrCreate"
        }));
        let mounts = agent_volume_mounts_json(Some(&tools));
        assert!(mounts.iter().any(|mount| {
            mount["name"] == REPO_CACHE_VOLUME
                && mount["mountPath"] == "/var/lib/centaur/repos"
                && mount["readOnly"] == true
        }));
    }

    #[test]
    fn tools_init_can_mount_repo_cache_from_pvc() {
        let mut tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        tools.repo_cache_path = Some("/var/lib/centaur/repos".to_owned());
        tools.repo_cache_pvc = Some("centaur-repo-cache".to_owned());

        let volumes = volumes_json(Some(&tools));
        let volume = volumes
            .iter()
            .find(|volume| volume["name"] == REPO_CACHE_VOLUME)
            .expect("repo-cache volume");

        assert_eq!(
            volume["persistentVolumeClaim"]["claimName"],
            "centaur-repo-cache"
        );
        assert_eq!(volume["persistentVolumeClaim"]["readOnly"], true);
        assert!(volume.get("hostPath").is_none());
    }

    #[test]
    fn public_repo_cache_scopes_host_path_to_public_projection() {
        let mut tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        tools.visibility = "public".to_owned();
        tools.repo_cache_path = Some("/var/lib/centaur/repos".to_owned());

        let scoped = tools.scoped_for_repo_cache_access(&RepoCacheAccess::Public);
        let volumes = volumes_json(Some(&scoped));
        assert!(volumes.iter().any(|volume| {
            volume["name"] == REPO_CACHE_VOLUME
                && volume["hostPath"]["path"] == "/var/lib/centaur/repos/public"
                && volume["hostPath"]["type"] == "DirectoryOrCreate"
        }));
        let mounts = agent_volume_mounts_json(Some(&scoped));
        assert!(mounts.iter().any(|mount| {
            mount["name"] == REPO_CACHE_VOLUME
                && mount["mountPath"] == "/var/lib/centaur/repos"
                && mount.get("subPath").is_none()
        }));
    }

    #[test]
    fn public_repo_cache_uses_pvc_subpath() {
        let mut tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        tools.visibility = "public".to_owned();
        tools.repo_cache_path = Some("/var/lib/centaur/repos".to_owned());
        tools.repo_cache_pvc = Some("centaur-repo-cache".to_owned());

        let scoped = tools.scoped_for_repo_cache_access(&RepoCacheAccess::Public);
        let volumes = volumes_json(Some(&scoped));
        let volume = volumes
            .iter()
            .find(|volume| volume["name"] == REPO_CACHE_VOLUME)
            .expect("repo-cache volume");
        assert_eq!(
            volume["persistentVolumeClaim"]["claimName"],
            "centaur-repo-cache"
        );
        assert!(volume.get("hostPath").is_none());

        for mount in [
            tools_init_container_json(&scoped, None)["volumeMounts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|mount| mount["name"] == REPO_CACHE_VOLUME)
                .expect("init repo-cache mount")
                .clone(),
            agent_volume_mounts_json(Some(&scoped))
                .into_iter()
                .find(|mount| mount["name"] == REPO_CACHE_VOLUME)
                .expect("agent repo-cache mount"),
        ] {
            assert_eq!(mount["mountPath"], "/var/lib/centaur/repos");
            assert_eq!(mount["subPath"], "public");
        }
    }

    #[test]
    fn public_repo_cache_filters_private_tool_sources() {
        let mut tools = ToolsConfig::new("acme/private-tools", "centaur-agent:test");
        tools.repo_cache_path = Some("/var/lib/centaur/repos".to_owned());
        tools.extra_sources.push(ToolSource {
            repo: "acme/public-tools".to_owned(),
            git_ref: Some("docs".to_owned()),
            source_subdir: "tools".to_owned(),
            visibility: "public".to_owned(),
        });
        tools.extra_sources.push(ToolSource {
            repo: "acme/other-private-tools".to_owned(),
            git_ref: None,
            source_subdir: "tools".to_owned(),
            visibility: "private".to_owned(),
        });

        let scoped = tools.scoped_for_repo_cache_access(&RepoCacheAccess::Public);

        assert_eq!(scoped.repo, "acme/public-tools");
        assert_eq!(scoped.git_ref.as_deref(), Some("docs"));
        assert!(scoped.extra_sources.is_empty());
        let script = tools_init_container_json(&scoped, None)["command"][2]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(script.contains("cache_repo=\"/var/lib/centaur/repos/acme/public-tools\""));
        assert!(!script.contains("acme/private-tools"));
        assert!(!script.contains("acme/other-private-tools"));
    }

    #[test]
    fn tools_init_with_token_wires_askpass_and_secret_volume() {
        let mut tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        tools.github_token = Some(GitHubTokenRef {
            secret_name: "centaur-repo-cache-github-token".to_owned(),
            secret_key: "token".to_owned(),
        });
        let c = tools_init_container_json(&tools, None);
        let script = c["command"][2].as_str().unwrap();
        assert!(script.contains("GIT_ASKPASS=/tmp/git-askpass"));
        assert!(script.contains("/tools-github-token/token"));
        let mounts = c["volumeMounts"].as_array().unwrap();
        assert_eq!(mounts.len(), 2);
        assert!(
            mounts
                .iter()
                .any(|m| m["mountPath"] == "/tools-github-token")
        );

        // The pod gets a secret-backed volume projecting the token to `token`.
        let volumes = volumes_json(Some(&tools));
        let token_vol = volumes
            .iter()
            .find(|v| v["name"] == GITHUB_TOKEN_VOLUME)
            .expect("token volume");
        assert_eq!(
            token_vol["secret"]["secretName"],
            "centaur-repo-cache-github-token"
        );
        assert_eq!(token_vol["secret"]["items"][0]["path"], "token");
    }

    #[test]
    fn volumes_without_token_are_just_emptydirs() {
        let tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        let volumes = volumes_json(Some(&tools));
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0]["name"], TOOLS_VOLUME);
        assert!(volumes[0]["emptyDir"].is_object());
    }

    #[test]
    fn agent_mounts_tools_writable_for_refresh() {
        let tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        let mounts = agent_volume_mounts_json(Some(&tools));
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0]["mountPath"], "/app/tools");
        assert!(mounts[0].get("readOnly").is_none());

        assert!(agent_volume_mounts_json(None).is_empty());
    }

    #[test]
    fn agent_mounts_token_for_private_repo_refresh() {
        let mut tools = ToolsConfig::new("paradigmxyz/centaur", "centaur-agent:test");
        tools.github_token = Some(GitHubTokenRef {
            secret_name: "centaur-repo-cache-github-token".to_owned(),
            secret_key: "token".to_owned(),
        });

        let env = agent_env(Some(&tools));
        assert!(env.contains(&("CENTAUR_TOOLS_AUTO_RELOAD".to_owned(), "true".to_owned())));
        assert!(env.contains(&(
            "CENTAUR_TOOLS_GITHUB_TOKEN_FILE".to_owned(),
            "/tools-github-token/token".to_owned()
        )));
        let mounts = agent_volume_mounts_json(Some(&tools));
        assert_eq!(mounts.len(), 2);
        assert!(mounts.iter().any(|mount| {
            mount["mountPath"] == "/tools-github-token" && mount["readOnly"] == true
        }));
    }
}
