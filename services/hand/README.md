# Hand image

`services/hand` builds the slim image a Centaur hand runs: a Linux box with a
shell, Git, GitHub CLI, and `centaur-hand`, which exposes nanocode's hand
protocol to a brain. It is not the sandbox image and has no harness, session
state, repo cache, or credentials of its own.

Derived images are the intended extension point: add the tools your principal
needs with `FROM centaur-hand:latest`, keep the full tini entrypoint
`["/usr/bin/tini","--","/entrypoint.sh"]`, and keep the `agent` user at
uid/gid 1001. A hand writes only inside its
workspace and `$HOME`.

## Environment contract

`centaur-hand` reads these variables (long flags of the same names also work):

| Variable | Default | Meaning |
|---|---|---|
| `CENTAUR_HAND_LISTEN` | `0.0.0.0:7070` | Hand protocol (plaintext WebSocket). |
| `CENTAUR_HAND_HEALTH_LISTEN` | `0.0.0.0:7071` | `GET /healthz`; 200 once the hand is serving, 503 before. |
| `CENTAUR_HAND_TOKEN_FILE` | `/var/run/centaur-hand/token` | Per-hand bearer token. The daemon refuses to start if it is missing or empty. |
| `CENTAUR_HAND_WORKSPACE` | `/home/agent/workspace` | Tool root: shell commands, reads, writes, and clones start here. |
| `CENTAUR_HAND_NAME` | hostname | Name the brain sees for this hand. |

The token file is the lease credential. It is read once at startup and never
logged. Do not pass it as an argument or bake it into an image.

`/entrypoint.sh` prepares the runtime before starting `centaur-hand`:

- it creates `$HOME/.centaur/env.sh` if absent and exports
  `BASH_ENV=$HOME/.centaur/env.sh`, so every non-interactive shell sources the
  lease's placeholder environment. `/etc/profile.d/centaur-env.sh` does the same
  for login shells;
- when `/etc/centaur-hand/ca.crt` exists, it writes a combined bundle to
  `$HOME/.centaur/ca-certificates.crt` and exports `SSL_CERT_FILE`,
  `REQUESTS_CA_BUNDLE`, `NODE_EXTRA_CA_CERTS`, `GIT_SSL_CAINFO`, and
  `CURL_CA_BUNDLE` to it. A hand runs as uid 1001, so it cannot edit the system
  trust store.

The image also configures Git in `/etc/gitconfig` to rewrite GitHub SSH URLs to
HTTPS and to use `gh auth git-credential`. The credential helper sends whatever
placeholder is in the environment; an egress proxy is expected to replace it
with the real credential on the wire. The image itself never contains a real
secret.

## Extending the image

```dockerfile
FROM centaur-hand:latest

USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends postgresql-client \
    && rm -rf /var/lib/apt/lists/*
USER agent

# Optional: additional CA certificates belong in the documented mount, not in
# the image, so one image works for every hand lease.
```

Keep the base image's full entrypoint (`/usr/bin/tini -- /entrypoint.sh`) and
uid/gid 1001 so the token file, environment hook, and writable trust bundle keep
working. Tini is PID 1 and forwards signals to `/entrypoint.sh` and
`centaur-hand`; replacing it with only `/entrypoint.sh` drops that signal
handling. Add language runtimes,
CLIs, or libraries in a derived image; do not add per-lease files or secrets.

## Building and running locally

From the repository root:

```bash
just _build-hand
install -d -m 0700 /tmp/hand-token
printf 'dev-token\n' > /tmp/hand-token/token
docker run --rm -p 7070:7070 -p 7071:7071 \
  -v /tmp/hand-token/token:/var/run/centaur-hand/token:ro \
  centaur-hand:latest
curl -fsS http://127.0.0.1:7071/healthz
```

The hand serves one bearer; the brain supplies it when attaching, and the pod
is deleted when the lease ends.
