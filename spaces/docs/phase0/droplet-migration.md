# Windows kind → DigitalOcean Droplet migration

Do this when always-on webhooks / OAuth callbacks / shared staff use are needed. Phase 0 kill-gate already passed on the laptop.

## Target

- Ubuntu LTS x86_64 Droplet
- **16 GB / 4–8 vCPU / 160–200 GB** (see [footprint.md](footprint.md)); bump to 32 GB when concurrent
- k3s + Docker + `just` + `kubectl` + `helm` + `jq`
- Cloudflare Tunnel or Tailscale Funnel for HTTPS

## Checklist

1. Create Droplet; harden SSH; install Docker, k3s, tooling.
2. Configure k3s local registry mirror if using `just up k3s` (see Centaur mac-mini / quickstart docs).
3. Clone the **same git commit** that passed Phase 0.
4. Recreate secrets intentionally:
   - Prefer **1Password** (`ironProxy.secretSource=onepassword`) before real tokens; or carry env mode briefly.
   - Generate Slack boot placeholders if Slack unused; set `ANTHROPIC_API_KEY` (or OpenAI) in the secret source.
   - Use Linux/`openssl rand` (no CRLF) — Windows Git Bash CR bug is documented in bootstrap script.
5. `CENTAUR_IMAGE_SOURCE=ghcr just up k3s` (or build locally if needed).
6. Health + Claude PONG smoke (same commands as Phase 0).
7. Re-run isolation script (principals A/B + grants + effective hosts + session bind).
8. Point tunnel at console (+ teamsbot later); update OAuth redirect URLs.
9. Idle or delete the laptop kind cluster so there is one source of truth.
10. Backups: Postgres PVC / etcd snapshot + documented rebuild from this checklist.

## Do not

- Start Azure / ISV credits
- Register HubSpot/M365 production OAuth clients until egress domain allowlist is non-wildcard ([egress.md](egress.md))
