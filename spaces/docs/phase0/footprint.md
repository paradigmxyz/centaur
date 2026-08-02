# Phase 0 footprint (kind on Windows laptop)

Measured 2026-08-02 against `kind-centaur`, namespace `centaur`, GHCR images, warm pool size 3, Claude harness.

## Node

| Metric | Value |
|--------|--------|
| Kind node container memory | ~3.4–3.5 GiB used (host limit ~31 GiB) |
| Node `free` used | ~3.5 GiB |
| Disk under node overlay | ~18 GiB used (images dominate) |
| Postgres PVC | 20 Gi bound, StorageClass `standard` |

## Images (largest)

| Image | Size |
|-------|------|
| `centaur-agent` | ~2.85 GB |
| `paradedb/paradedb:0.23.0-pg16` | ~524 MB |
| `centaur-console` | ~261 MB |
| `centaur-api-rs` | ~166 MB |
| Other control-plane images | &lt; 100 MB each |

## Running shape

- Control plane: api-rs, console, console-worker, postgres, slackbotv2, repo-cache
- Warm sandboxes: 3 agent + 3 iron-proxy pairs (plus leftovers until reaped)
- Chart resource requests/limits: unset (`resources: {}`) — BestEffort QoS
- `kubectl top` unavailable (no metrics-server in this kind cluster)

## Sizing takeaway for Droplet

Until a quieter idle measurement with warm pool 1:

- **Minimum practical:** 8 GB RAM / 4 vCPU / 80+ GB disk (tight; expect image pull + one sandbox only)
- **Comfortable Phase 1–3:** 16 GB RAM / 4–8 vCPU / 160–200 GB disk
- **Matches v3 target:** 32 GB / 8 vCPU when concurrent users appear

Re-measure on the Droplet after `just up k3s` with `SESSION_SANDBOX_WARM_POOL_SIZE=1` for idle baseline.
