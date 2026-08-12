# Configurable Git Clone URLs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Secret-backed HTTP/HTTPS Git clone URLs for repo-cache, overlays, and tool delivery without breaking GitHub `owner/name` deployments.

**Architecture:** Helm keeps `repo` as the stable identity and propagates optional `cloneUrl` values as structured data. The Python repo-cache and Rust tool-source modules resolve an explicit clone URL or derive the existing GitHub URL, while generic `GIT_ASKPASS` credentials stay in mounted Secret files.

**Tech Stack:** Helm templates and JSON schema, Python 3.11+, Rust/Clap/Serde, Kubernetes Secrets and NetworkPolicies, Git.

## Global Constraints

- Plain `http://` clone URLs are supported and documented as transmitting credentials and repository content without transport encryption.
- Tokens never appear in URLs, environment values, logs, tests, or committed files.
- `repo` remains the cache key, mount path, and `AGENT_REPO` value.
- Missing `cloneUrl` preserves `https://github.com/<repo>.git`.
- Existing `githubToken` configuration remains compatible; generic credentials take precedence.
- Existing unrelated worktree changes must remain untouched.

---

### Task 1: Repo-cache Clone URL Resolution

**Files:**
- Modify: `services/sandbox/test_repo_cache_sync.py`
- Modify: `services/sandbox/repo_cache_sync.py`

**Interfaces:**
- Consumes: `REPOSITORIES`, new JSON `REPOSITORY_CLONE_URLS`, `GIT_USERNAME`, and `GIT_TOKEN_FILE` environment values.
- Produces: `RepoCacheSync.repository_clone_url(repo) -> str` and URL hashes in the readiness fingerprint.

- [ ] **Step 1: Write failing Python tests**

Add focused tests showing that an explicit HTTP URL is selected, GitHub remains the fallback, embedded URL credentials are rejected, `GIT_USERNAME` is written into askpass, and readiness contains only SHA-256 URL digests.

- [ ] **Step 2: Verify the new tests fail**

Run:

```bash
uv run python -m unittest discover -s services/sandbox -p 'test_repo_cache_sync.py' -v
```

Expected: failures because clone URL parsing, generic credentials, and URL hashes do not exist.

- [ ] **Step 3: Implement minimal repo-cache support**

Parse `REPOSITORY_CLONE_URLS` as a JSON object, derive the GitHub fallback in one helper, reject URL user-info, use the resolved URL for clone/update, and add deterministic SHA-256 hashes to `repository_fingerprint()`. Read the credential username and token path through generic names with legacy fallbacks.

- [ ] **Step 4: Verify Python tests pass**

Run the Step 2 command and expect all repo-cache tests to pass.

- [ ] **Step 5: Commit the Python slice**

```bash
git add services/sandbox/repo_cache_sync.py services/sandbox/test_repo_cache_sync.py
git commit -m "feat: support custom repo-cache clone URLs"
```

### Task 2: Helm Source Normalization And Credentials

**Files:**
- Modify: `contrib/chart/values.yaml`
- Modify: `contrib/chart/values.schema.json`
- Modify: `contrib/chart/templates/_helpers.tpl`
- Modify: `contrib/chart/templates/repo-cache.yaml`
- Modify: `contrib/chart/templates/repo-cache-secret.yaml`
- Modify: `contrib/chart/templates/apirs.yaml`

**Interfaces:**
- Consumes: `cloneUrl` on repository source objects and `gitCredentials` on `repoCache` and `toolServer`.
- Produces: `REPOSITORY_CLONE_URLS`, `GIT_USERNAME`, `GIT_TOKEN_FILE`, `KUBERNETES_TOOLS_CLONE_URL`, tool-source JSON, and generic Secret references.

- [ ] **Step 1: Add a failing Helm render assertion script**

Render the chart with a temporary values file containing a GitLab HTTP source and generic Secret. Assert with `yq`/`jq` or YAML-safe text checks that the repo-cache receives JSON clone URLs, username, token file mount, port 82, and api-rs receives the tool clone URL. Render a conflicting duplicate URL and require Helm to fail.

- [ ] **Step 2: Verify the render assertions fail**

Run the script or equivalent commands and expect missing fields or schema rejection.

- [ ] **Step 3: Implement Helm propagation**

Carry `cloneUrl` through `centaur.overlaySources`, collect repo-cache URL mappings, render generic credential mounts with legacy fallback, add the primary URL and extra-source URLs to api-rs, and extend the values schema. Fail rendering when one `repo` has conflicting explicit URLs.

- [ ] **Step 4: Verify rendering and lint**

Run:

```bash
helm lint contrib/chart
helm template centaur contrib/chart -f /tmp/centaur-gitlab-values.yaml > /tmp/centaur-gitlab-rendered.yaml
```

Expected: lint succeeds and all assertions pass.

- [ ] **Step 5: Commit the Helm slice**

```bash
git add contrib/chart
git commit -m "feat: configure non-GitHub repository sources"
```

### Task 3: Rust Tool Source Transport

**Files:**
- Modify: `services/api-rs/crates/centaur-sandbox-agent-k8s/src/tools.rs`
- Modify: `services/api-rs/crates/centaur-sandbox-agent-k8s/src/lib.rs`
- Modify: `services/api-rs/crates/centaur-api-server/src/args.rs`

**Interfaces:**
- Consumes: optional `clone_url` per `ToolSource` and a generic Git credential Secret reference with username.
- Produces: correctly quoted clone commands for api-rs and sandbox init containers, with both HTTP and HTTPS proxy variables.

- [ ] **Step 1: Write failing Rust tests**

Add tests that primary and extra sources preserve custom HTTP clone URLs, GitHub is the fallback, the clone URL is shell-quoted, public filtering retains the URL, generic usernames appear in askpass, and `HTTP_PROXY` accompanies `HTTPS_PROXY`.

- [ ] **Step 2: Verify the new Rust tests fail**

Run in a Rust-capable container/build environment:

```bash
cargo test -p centaur-sandbox-agent-k8s
cargo test -p centaur-api-server tool_source
```

Expected: compile/test failures because the new fields and arguments are absent.

- [ ] **Step 3: Implement minimal Rust support**

Add `clone_url: Option<String>` to `ToolsConfig`/`ToolSource`, resolve the fallback in one method, replace GitHub-specific credentials with a username-bearing generic credential structure while accepting legacy args, quote generated shell values, and use the URL in api-rs source synchronization.

- [ ] **Step 4: Verify Rust tests and formatting**

Run:

```bash
cargo fmt --all --check
cargo test -p centaur-sandbox-agent-k8s
cargo test -p centaur-api-server tool_source
```

Expected: all selected tests pass with no formatting diff.

- [ ] **Step 5: Commit the Rust slice**

```bash
git add services/api-rs/crates/centaur-sandbox-agent-k8s services/api-rs/crates/centaur-api-server
git commit -m "feat: propagate custom tool clone URLs"
```

### Task 4: Operator Documentation

**Files:**
- Modify: `docs/pages/extend/overlay.mdx`
- Modify: `docs/pages/reference/configuration.mdx`
- Modify: `contrib/scripts/bootstrap-k8s-secrets.sh` only if its help text must describe the generic Secret path.

**Interfaces:**
- Consumes: the final Helm value names and precedence rules.
- Produces: a complete self-hosted GitLab HTTP example with a prominent plaintext warning.

- [ ] **Step 1: Document source and credential configuration**

Show `repo`, `cloneUrl`, `gitCredentials`, `egressPorts`, Secret creation, and `AGENT_REPO` path semantics using neutral example hosts and placeholder tokens.

- [ ] **Step 2: Check documentation for leaks and stale GitHub-only claims**

Run:

```bash
rg -n "owner/name on GitHub|GitHub repositories|githubToken" docs/pages/extend/overlay.mdx docs/pages/reference/configuration.mdx contrib/chart/values.yaml
rg -n "192\.168\.|oauth2:.*@|PRIVATE-TOKEN" docs contrib/chart
```

Expected: remaining GitHub-specific text describes fallback/compatibility only; no private host or credential appears.

- [ ] **Step 3: Commit documentation**

```bash
git add docs/pages/extend/overlay.mdx docs/pages/reference/configuration.mdx contrib/scripts/bootstrap-k8s-secrets.sh
git commit -m "docs: explain self-hosted Git repository sources"
```

### Task 5: Integrated Verification

**Files:**
- No production files expected.

**Interfaces:**
- Consumes: all prior tasks.
- Produces: fresh evidence for compatibility and HTTP clone behavior.

- [ ] **Step 1: Run focused and broad automated checks**

```bash
uv run python -m unittest discover -s services/sandbox -p 'test_*.py'
helm lint contrib/chart
git diff --check
```

Run Rust formatting and affected tests through the repository's Docker build path when the host lacks Cargo.

- [ ] **Step 2: Prove GitHub compatibility rendering**

Render default/dev values and confirm derived GitHub URLs and legacy token settings still map to the same repository identities and Secret keys.

- [ ] **Step 3: Prove HTTP Git behavior in kind**

Verify `kubectl config current-context` is `kind-centaur`, start an ephemeral HTTP Git endpoint reachable from the node, deploy repo-cache with a custom `cloneUrl` and allowed HTTP port, then verify the cache checkout commit and `.repo-cache-ready` URL hash. Do not print the credential.

- [ ] **Step 4: Review repository state**

```bash
git status --short
git log --oneline --decorate -8
git diff origin/main...HEAD --check
```

Expected: only scoped feature/docs changes are present and all checks pass.
