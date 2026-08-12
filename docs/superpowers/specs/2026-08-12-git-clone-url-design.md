# Configurable Git Clone URLs

## Objective

Allow Centaur repo-cache, overlays, and tool delivery to clone repositories
from an operator-supplied HTTP or HTTPS Git URL, including self-hosted GitLab,
while preserving the existing GitHub `owner/name` behavior.

The initial deployment target is a trusted-LAN GitLab instance served over
plain HTTP. Centaur will permit this explicitly. Documentation must warn that
credentials and repository contents are not encrypted in transit on HTTP.

## Configuration Interface

Repository identity and transport are separate:

- `repo` is the stable cache identity and mount path, such as
  `group/project`. It remains visible at
  `/home/agent/github/group/project` and is the value used by `AGENT_REPO`.
- `cloneUrl` is the remote passed to Git, such as
  `http://git.example.internal:82/group/project.git`.
- When `cloneUrl` is absent, Centaur derives
  `https://github.com/<repo>.git`, preserving current deployments.

The following source shapes accept `cloneUrl`:

```yaml
toolServer:
  enabled: true
  repo: example/centaur
  cloneUrl: https://github.com/example/centaur.git
  extraSources:
    - repo: platform/extra-tools
      cloneUrl: http://git.example.internal:82/platform/extra-tools.git

overlays:
  sources:
    - repo: platform/agent-overlay
      cloneUrl: http://git.example.internal:82/platform/agent-overlay.git
      ref: main
      visibility: private

repoCache:
  enabled: true
  repositories:
    - repo: group/project
      cloneUrl: http://git.example.internal:82/group/project.git
      ref: main
      visibility: private
  egressPorts: [82, 443]
```

One `repo` identity must resolve to one clone URL in a rendered release.
Conflicting explicit URLs are a Helm rendering error. An explicit URL wins over
the derived GitHub fallback for the same identity.

## Credential Interface

Generic credentials are configured independently for repo-cache and direct
tool clones:

```yaml
repoCache:
  gitCredentials:
    username: oauth2
    existingSecretName: gitlab-repo-token
    secretKey: token

toolServer:
  gitCredentials:
    username: oauth2
    existingSecretName: gitlab-repo-token
    secretKey: token
```

`username` is not secret. The password/token is read from a Kubernetes Secret
file and supplied to Git through `GIT_ASKPASS`. It must not be placed in an
environment variable, command-line argument, rendered URL, Pod annotation, or
log message.

Existing `repoCache.githubToken` and `toolServer.githubToken` values remain
supported. When the corresponding `gitCredentials.existingSecretName` is set,
the generic configuration takes precedence. Legacy GitHub credentials use the
username `x-access-token`.

Centaur rejects clone URLs containing URL user-info, for example
`http://user:token@host/repo.git`, because those URLs are persisted in Helm
release data and can appear in Git errors. Operators must use the Secret-backed
credential interface.

## Data Flow

### Repo-cache enabled

1. Helm normalizes `repoCache.repositories`, `overlays.sources`, and the
   compatibility `toolServer` source into repositories keyed by `repo`.
2. Helm sends a JSON object of explicit clone URLs to the repo-cache container
   in `REPOSITORY_CLONE_URLS`.
3. The repo-cache sync process resolves each URL, falling back to GitHub,
   configures `GIT_ASKPASS`, and clones or updates the checkout.
4. The checkout remains stored under its stable `repo` path. API and sandbox
   mounts therefore do not depend on the Git host or URL.
5. The readiness fingerprint includes a SHA-256 digest of each resolved clone
   URL. Changing a URL invalidates stale readiness without writing the URL to
   the readiness file.
6. If an existing checkout's `origin` differs from the resolved clone URL,
   repo-cache builds a fresh checkout in a temporary directory and replaces the
   old checkout only after clone, fetch, and checkout succeed. Objects from the
   old remote are never reused, while a transient new-remote failure leaves the
   last checkout available and clears readiness.

### Repo-cache disabled

1. Helm sends the primary tool source clone URL and additional source JSON to
   api-rs.
2. api-rs uses the clone URL for its tool metadata checkout.
3. api-rs passes the same clone URL and generic credential reference into each
   sandbox tools configuration.
4. The sandbox `tools-bootstrap` init container clones through the paired
   iron-proxy. Both `HTTP_PROXY` and `HTTPS_PROXY` are set so HTTP and HTTPS
   remotes follow the same egress path.
5. For a clone URL with a literal IP, Centaur adds an exact host CIDR and the
   configured port to iron-proxy egress, scoped to the sandbox's repository
   access (`none`, `public`, or `all`). DNS names do not create broad public
   custom-port rules. Private DNS remotes, and DNS remotes on ports other than
   the proxy's baseline HTTPS port, require repo-cache because their changing
   addresses cannot be represented by a stable narrow rule.

## Security And Error Handling

- Plain HTTP is allowed only because the operator explicitly supplies an
  `http://` URL. Documentation states that the token and code are observable on
  the network.
- The token file is mounted read-only with mode `0400` and never copied into
  the published tool tree.
- Generated shell scripts quote clone URLs as data. A URL cannot inject shell
  syntax.
- Error messages identify the stable `repo`, not the clone URL.
- Direct clone Git stderr is suppressed so transport errors cannot echo the URL;
  Centaur emits a stable repo-based failure message after bounded retries.
- The repo-cache NetworkPolicy remains port-based. Operators must add the
  custom Git port to `repoCache.egressPorts`.
- Direct sandbox clones remain subject to iron-proxy upstream policy. Enabling
  a clone URL does not grant general sandbox egress.
- Invalid clone URL configuration fails before a repository is marked ready.

## Compatibility

- Existing string entries in `repoCache.repositories` continue to work.
- Existing object entries without `cloneUrl` continue to use GitHub.
- Existing `toolServer.repo`, `toolServer.extraSources`, and
  `overlays.sources` continue to render identically without `cloneUrl`.
- Existing GitHub token settings and generated Secret behavior remain intact.
- Cache and sandbox paths do not change.

## Verification

Automated verification covers:

- Python parsing, GitHub fallback, custom HTTP URL selection, URL digest
  readiness, credential username, and rejection of embedded credentials.
- Rust source parsing and generated clone scripts for primary and extra tool
  sources, HTTP proxy export, shell quoting, and generic Secret mounting.
- Helm schema and rendering for custom clone URLs, generic credentials,
  compatibility values, conflicting URLs, and custom egress ports.
- Existing sandbox Python tests, affected Rust crate tests, Helm lint, and
  formatting checks.

The local kind proof uses an HTTP Git endpoint reachable from the kind node and
verifies the checked-out commit and readiness sentinel. A real private GitLab
proof additionally requires a concrete project path and a token supplied as a
Kubernetes Secret; no token is committed or printed.
