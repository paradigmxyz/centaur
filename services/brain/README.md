# Brain

`centaur-brain` runs nanocode agent turns outside a sandbox. It is the harness
service behind the `nanocode` harness type: api-rs creates a cheap logical
"brain sandbox" per nanocode session and talks to the service over HTTP and a
WebSocket, so the existing stdout pump, event persistence, terminal detection,
and rendering work unchanged. It is off by default (`brain.enabled`).

## HTTP

The service listens on `0.0.0.0:8790`:

```bash
centaur-brain serve --listen 0.0.0.0:8790
```

| Route | Purpose |
|-------|---------|
| `GET /healthz` | Readiness and liveness. |
| `POST /v1/sandboxes` | Create (idempotently) a logical sandbox. |
| `GET /v1/sandboxes/{id}` | Status; `running` while the sandbox exists. |
| `DELETE /v1/sandboxes/{id}` | Cancel the active turn and drop its buffers. |
| `GET /v1/sandboxes/{id}/io` | WebSocket carrying the blocks protocol in and Codex app-server notifications out, translated from nanocode events by harness-server's turn normalizer. |
| `GET /v1/sandboxes/{id}/output?since_ms=N` | Buffered output lines for replay. |

At most one socket attaches to a sandbox; a new attach replaces the old one.
Detaching does not cancel a running turn, and output produced while detached is
buffered (in memory, the last 10k lines).

## Environment

- `BRAIN_DATABASE_URL` — optional Postgres DSN for nanocode durability. The
  role's default search_path is the `nanocode` schema (see the
  `centaur_brain` role migration in `crates/centaur-session-sqlx/migrations`).
  When unset the service keeps state in memory, which is for local development
  only. Provision a login for the role out of band
  (`ALTER ROLE centaur_brain LOGIN PASSWORD ...`) and store the DSN in a Secret;
  the Helm value is `brain.databaseUrl.existingSecretName`.
- `BRAIN_DEFAULT_MODEL` — default model for turns that name none; defaults to
  `luna`.
- `BRAIN_DEFAULT_THINKING` — default reasoning effort; defaults to `low`.
- `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` — model provider credentials. The chart
  sets the placeholders `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` (the same
  literal strings) unless `brain.extraEnv` overrides them: behind a proxy those
  placeholders are what iron-proxy substitutes on the wire, and a direct key can
  be supplied through `extraEnv` instead.
- `OPENAI_BASE_URL` — optional API base override, from `brain.openaiBaseUrl`.
  Empty uses the provider default, `https://api.openai.com/v1`, and nanocode
  calls its Responses API over HTTPS with SSE (`{base}/responses`,
  `Accept: text/event-stream`).
- `HTTPS_PROXY`, `SSL_CERT_FILE`, `SSL_CERT_DIR`, `NO_PROXY` — honored
  process-wide, so the service can reach model providers through an egress
  proxy. nanocode does not merge these over the platform store: when
  `SSL_CERT_FILE` or `SSL_CERT_DIR` is set it loads trust roots only from those
  paths. `brain.trustReleaseCa` therefore keeps `SSL_CERT_FILE` on the image's
  public bundle (`/etc/ssl/certs/ca-certificates.crt`, 148 roots) and adds the
  release CA through `SSL_CERT_DIR=/etc/centaur-ca`; nanocode reads every regular
  PEM file in that directory and unions the two sets. A single concatenated file
  built by an init container would also work, but this needs no writable volume,
  no extra container, and no file assembly.

This spike sends model traffic directly over HTTPS. Production model egress
should go through a dedicated iron-proxy with a model-only principal; that is a
follow-up, not part of this deployment wiring.

## Chart

The chart renders a Deployment, Service, and NetworkPolicies when
`brain.enabled=true` and points api-rs at the Service with
`CENTAUR_BRAIN_URL`. The brain is never reaped by the sandbox reaper, so it
stays a long-lived service rather than part of a warm pool.

`brain.trustReleaseCa=true` mounts `firewall.existingCaSecretName` (the public
`ca-cert.pem` entry; the private key lives in a separate Secret and is never
projected) at `/etc/centaur-ca` and sets the two trust variables above. This is
how the e2e stack makes provider hostnames — which CoreDNS rewrites to its fake
model server — verify: that server's certificate is signed by the release CA.
