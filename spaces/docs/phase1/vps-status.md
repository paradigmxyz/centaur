# CubePath VPS status (solo always-on)

Updated 2026-08-02.

## Host

| Field | Value |
|-------|--------|
| Provider | [CubePath](https://cubepath.com/) General Purpose |
| Instance | `ubuntu-gp-small-mia-01` (`vps26630.cubepath.net`) |
| Specs | `gp.small` — 8 vCPU / 16 GB / ~200 GB |
| IPv4 | `157.254.174.172` |
| OS | Ubuntu 24.04 |
| SSH | `ssh -i ~/.ssh/id_ed25519 root@157.254.174.172` |
| Checkout | `/opt/MD-OPS` @ `feat/spaces-phase1-oauth-scaffold` |

## Proven on this host

- [x] Bootstrap (`spaces/docs/phase1/droplet-bootstrap.sh`) — Docker, k3s, helm, just, jq
- [x] `ironProxy.secretSource: env` + Spaces allowlist overlay
- [x] `CENTAUR_IMAGE_SOURCE=ghcr just up`
- [x] `ANTHROPIC_API_KEY` in `centaur-infra-env` (not the placeholder)
- [x] `just smoke` → **SMOKE_OK** (Claude / `claudecode` PONG)

## Not done yet (needs Cloudflare Tunnel)

- [ ] Public HTTPS to console (OAuth callbacks)
- [ ] Public HTTPS to teamsbot `/api/messages`
- [ ] HubSpot + Entra app registration with those callback URLs
- [ ] Teamsbot enablement + two-user E2E
- [ ] Re-run Phase 0 isolation proof on this host (can do without tunnel; optional)

## Tomorrow — Cloudflare Tunnel (checklist)

Do this on the VPS after installing `cloudflared` (named tunnel preferred over quick tunnels).

1. Create a Cloudflare Tunnel bound to a hostname you control (example placeholders):
   - `console.<your-domain>` → `http://centaur-centaur-console.centaur.svc.cluster.local:3000`  
     (or `kubectl port-forward` / ingress — match whatever chart expose path you choose)
   - `teams.<your-domain>` → teamsbot service port for `/api/messages`
2. Set `CENTAUR_CONSOLE_PUBLIC_URL` to the public console host (no trailing slash).
3. Follow [`oauth-app-registration.md`](oauth-app-registration.md) with those URLs.
4. Redeploy / restart console + teamsbot so they see the public URL.
5. Re-run `just smoke` after any chart change (still works in-cluster without the tunnel).

Exact in-cluster Service names:

```bash
kubectl -n centaur get svc
```

## Solo VPS tip (16 GB)

Chart default warm pool is 3 sandboxes. On this host, prefer **1**:

```bash
cd /opt/MD-OPS
# merge into values.local.yaml (see contrib/chart/values.vps.example.yaml):
#   apiRs:
#     sandboxWarmPoolSize: 1
export CENTAUR_IMAGE_SOURCE=ghcr
export CENTAUR_EXTRA_VALUES=contrib/chart/values.local.yaml,contrib/chart/values.spaces.yaml
just deploy
```

## Secrets reminder

- Dummy Slack / OP boot vars are fine until real OAuth.
- Do not commit `.env` or `values.local.yaml`.
- Prefer 1Password before putting many real third-party tokens on the VPS.
