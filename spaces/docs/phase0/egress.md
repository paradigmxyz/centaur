# Phase 0 egress posture

## Verified

1. **Namespace default-deny NetworkPolicy** is installed (`centaur-centaur-default-deny`, empty `podSelector`, Ingress+Egress).
2. **Sandbox pods** set `HTTPS_PROXY` / `HTTP_PROXY` to their dedicated iron-proxy Service (not direct internet).
3. **Per-sandbox iron-proxy** runs in **managed mode** (`IRON_CONTROL_PLANE_URL` set): no local `proxy.yaml`; config comes from the control plane sync.
4. **Upstream deny CIDRs** on this laptop include kind ranges (`10.244.0.0/16`, `10.96.0.0/12`) via [`contrib/chart/values.local.yaml`](../../../contrib/chart/values.local.yaml).

## Still open (domain allowlist)

Shipped iron-proxy base config still uses a permissive domain allowlist (`domains: ["*"]`) in [`services/iron-proxy/iron-proxy.yaml`](../../../services/iron-proxy/iron-proxy.yaml). Managed mode does not replace that with a tight list unless we change the base fragment registration / rebuild the image.

**Before registering real HubSpot / M365 / client credentials**, lock domains to an explicit list. Draft for Phase 0–1 Claude-only use:

```yaml
transforms:
  - name: allowlist
    config:
      domains:
        - "api.anthropic.com"
        - "*.anthropic.com"
        - "api.openai.com"      # only if Codex returns
        - "*.openai.com"
```

Apply path (pick one when locking for real):

1. Edit `services/iron-proxy/iron-proxy.yaml`, rebuild `centaur-iron-proxy`, load into kind (`kind load docker-image`) or push to the Droplet registry, recycle sandbox/proxy pods; or
2. Prefer upstream/control-plane managed allowlist if/when exposed as a first-class chart value (do not invent a parallel mechanism).

## Policy for Spaces

- No production OAuth client secrets until the domain allowlist is non-wildcard.
- Tool secret `hosts` rules remain a second line of defense; they do not replace the proxy allowlist.
