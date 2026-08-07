# Phase 0 egress posture

## Verified

1. **Namespace default-deny NetworkPolicy** is installed (`centaur-centaur-default-deny`, empty `podSelector`, Ingress+Egress).
2. **Sandbox pods** set `HTTPS_PROXY` / `HTTP_PROXY` to their dedicated iron-proxy Service (not direct internet).
3. **Per-sandbox iron-proxy** runs in **managed mode** (`IRON_CONTROL_PLANE_URL` set): no local `proxy.yaml`; config comes from the control plane sync.
4. **Upstream deny CIDRs** on this laptop include kind ranges (`10.244.0.0/16` and `10.96.0.0/12`) via [`contrib/chart/values.local.yaml`](../../../contrib/chart/values.local.yaml).

## Domain allowlist (managed mode)

Managed proxies **ignore** the baked [`iron-proxy.yaml`](../../../services/iron-proxy/iron-proxy.yaml) allowlist. Console emits an `allowlist` transform on proxy sync when:

```text
CENTAUR_IRON_PROXY_ALLOWLIST_DOMAINS=api.anthropic.com,*.anthropic.com,...
```

Chart wiring: `ironProxy.allowlistDomains` → that env on console + console-worker.
Spaces overlay: [`contrib/chart/values.spaces.yaml`](../../../contrib/chart/values.spaces.yaml).

```bash
# Example EXTRA_VALUES (gitignored local + committed Spaces lock)
export CENTAUR_EXTRA_VALUES=contrib/chart/values.local.yaml,contrib/chart/values.spaces.yaml
just deploy
# Restart console so new env is picked up; proxies re-sync within ~5s.
```

Phase 1 Spaces list (Claude + HubSpot + Graph + git):

- `api.anthropic.com`, `*.anthropic.com`
- `api.openai.com`, `*.openai.com` (optional Codex path)
- `api.hubapi.com`, `api.hubspot.com`
- `graph.microsoft.com`
- `github.com`, `api.github.com`, `*.githubusercontent.com`

Empty `allowlistDomains` keeps today’s open egress (no allowlist transform).

The unmanaged-mode baked YAML is tightened to the same baseline for image rebuilds; managed deployments must still set the chart value.

## Policy for Spaces

- No production OAuth client secrets until `ironProxy.allowlistDomains` is non-empty and deployed.
- Tool secret `hosts` rules remain a second line of defense; they do not replace the proxy allowlist.
