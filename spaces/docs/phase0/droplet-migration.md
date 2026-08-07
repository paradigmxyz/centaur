# Windows kind → always-on Linux VPS migration

Do this when always-on webhooks / OAuth callbacks / shared staff use are needed. Phase 0 kill-gate already passed on the laptop.

## Preferred host

**[CubePath](https://cubepath.com/) Cloud VPS — General Purpose** (Ubuntu LTS x86_64 + k3s). DigitalOcean is optional and not required; the Centaur stack only cares about RAM/CPU/disk and Ubuntu, not the brand.

| Stage | CubePath SKU (approx.) | Specs |
|-------|------------------------|-------|
| Solo / personal always-on | `gp.small` (~$31/mo) | **16 GB / 8 vCPU / 200 GB** |
| Team pilot | resize to `gp.medium` (~$57/mo) | **32 GB / 12 vCPU / 300 GB** |

Do not use ≤4 GB plans. An 8 GB `gp.starter` is a cost stretch only and may OOM under sandbox load.

Sizing is **provider-agnostic**: the same 16 GB floor / 32 GB team tier applies if you later use DigitalOcean or another VPS. Resizing is a panel action; Centaur config does not change.

Also need: Cloudflare Tunnel or Tailscale Funnel for HTTPS.

## Checklist

1. ~~Create the VPS; harden SSH; install Docker, k3s, and tooling~~ — done on CubePath `gp.small` (see [`../phase1/vps-status.md`](../phase1/vps-status.md)).
2. Configure k3s local registry mirror if using `just up k3s` (skipped when `CENTAUR_IMAGE_SOURCE=ghcr`).
3. ~~Clone the approved revision~~ — `/opt/MD-OPS` on the VPS.
4. ~~Recreate secrets (env mode + Anthropic)~~ — dummy Slack/OP boot vars; real `ANTHROPIC_API_KEY` in `centaur-infra-env`.
5. ~~`CENTAUR_IMAGE_SOURCE=ghcr just up`~~ — done.
6. ~~Health + Claude PONG smoke~~ — `SMOKE_OK` on CubePath (2026-08-02).
7. Re-run isolation script (principals A/B + grants + effective hosts + session bind) — optional before team use.
8. Point Cloudflare Tunnel at console (+ teamsbot later); update OAuth redirect URLs — **next**.
9. Idle or delete the laptop kind cluster so there is one source of truth.
10. Backups: Postgres PVC / etcd snapshot + documented rebuild from this checklist.
11. When inviting the team: resize VPS to the 32 GB class (`gp.medium` on CubePath).

Overlay template (no secrets): [`contrib/chart/values.vps.example.yaml`](../../../contrib/chart/values.vps.example.yaml).

## Do not

- Start Azure / ISV credits
- Assume DigitalOcean is required
- Register HubSpot/M365 production OAuth clients until egress domain allowlist is non-wildcard ([egress.md](egress.md))
