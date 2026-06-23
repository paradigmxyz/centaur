---
title: Expose the Slackbot with Tailscale Funnel
description: Publicly expose Centaur's Slackbot for Slack webhooks using the Tailscale Kubernetes operator and Funnel, with TLS terminated by Tailscale.
---

# Expose the Slackbot with Tailscale Funnel

Slack delivers events to the Slackbot over public HTTPS (`/api/webhooks/slack`).
The Slackbot listens on plain HTTP (port 3001) as an in-cluster `ClusterIP`
service and does not terminate TLS itself. For a durable, in-cluster way to make
it reachable from Slack, use the [Tailscale Kubernetes operator](https://tailscale.com/kb/1236/kubernetes-operator)
with [Funnel](https://tailscale.com/kb/1223/funnel): the operator publishes a
public endpoint at `https://<name>.<tailnet>.ts.net`, terminates TLS with an
auto-renewed Let's Encrypt certificate, and forwards plain HTTP to the Slackbot.

This is the production-style alternative to the ad-hoc laptop tunnel in
[Mac Mini-style setup](/mac-mini-setup#6-optional-expose-local-slackbot-with-a-tunnel)
(`kubectl port-forward` + `cloudflared`/`tailscale funnel`), which is fine for
quick local testing but ephemeral.

:::warning[The Slackbot has no inbound TLS of its own]
The chart never provisions a TLS certificate for the Slackbot — it is plain HTTP
on port 3001 by design. Tailscale Funnel (or any TLS-terminating proxy) owns the
public certificate.
:::

## Prerequisites

- Funnel enabled for your tailnet: in the Tailscale admin console DNS page, enable
  **MagicDNS** and **HTTPS certificates**.
- A tailnet policy (ACL) that defines the operator tags and grants Funnel to the
  operator's proxy nodes:
  ```jsonc
  {
    "tagOwners": { "tag:k8s-operator": [], "tag:k8s": ["tag:k8s-operator"] },
    "nodeAttrs": [ { "target": ["tag:k8s"], "attr": ["funnel"] } ]
  }
  ```
  Target `tag:k8s` (the operator's default proxy tag), **not** `autogroup:member`:
  tagged proxy nodes are not members, so the default Funnel grant would not cover
  them and the device would come up tailnet-only.
- An [OAuth client](https://tailscale.com/kb/1215/oauth-clients) for the operator
  (scopes `devices:core` and `auth_keys`, owner `tag:k8s-operator`).
- The Tailscale operator installed in the `tailscale` namespace:
  ```bash
  helm repo add tailscale https://pkgs.tailscale.com/helmcharts && helm repo update
  helm upgrade --install tailscale-operator tailscale/tailscale-operator \
    -n tailscale --create-namespace \
    --set-string oauth.clientId=<id> --set-string oauth.clientSecret=<secret> --wait
  ```

## Configure the chart

Expose the Slackbot with a Tailscale **Funnel Ingress**. A ready-to-use sample
lives at `contrib/chart/values.tailscale-funnel.example.yaml`:

```yaml
ingress:
  enabled: true
  className: tailscale
  defaultBackend: true            # the operator's Funnel Ingress expects a single backend
  annotations:
    tailscale.com/funnel: "true"  # public Funnel exposure; omit for tailnet-only
  tls:
    - hosts:
        - centaur-slackbotv2      # -> https://centaur-slackbotv2.<your-tailnet>.ts.net

networkPolicy:
  ingressControllerNamespaces:
    - kube-system
    - tailscale
```

What each piece does:

- `ingress.defaultBackend: true` makes the chart emit a single `spec.defaultBackend`
  (instead of host/path rules) — the shape the Tailscale operator's Funnel Ingress
  expects.
- `ingress.className: tailscale` routes the Ingress to the operator.
- The `tailscale.com/funnel: "true"` annotation makes the endpoint public. Omit it
  to keep the Slackbot reachable only inside your tailnet.
- `tls.hosts[0]` sets the device's MagicDNS name (`<name>.<tailnet>.ts.net`).
- Adding `tailscale` to `networkPolicy.ingressControllerNamespaces` lets the
  operator's proxy pods reach the Slackbot on port 3001. The Slackbot
  NetworkPolicy otherwise admits only the API, workflow-run pods, and the listed
  ingress-controller namespaces (default `kube-system`).

## Deploy

Layer the example file on top of your normal values with the `CENTAUR_EXTRA_VALUES`
hook, which keeps the shared `values.dev.yaml` untouched:

```bash
CENTAUR_EXTRA_VALUES=contrib/chart/values.tailscale-funnel.example.yaml just up
```

Or with Helm directly:

```bash
helm upgrade --install centaur contrib/chart -n centaur \
  -f contrib/chart/values.dev.yaml \
  -f contrib/chart/values.tailscale-funnel.example.yaml
```

## Point Slack at it

Set the Slack app's Event Subscriptions **Request URL** to:

```text
https://<name>.<tailnet>.ts.net/api/webhooks/slack
```

Then finish the Slack app in
[Deploying in Production → Configure Slack](/deploying-in-production#4-configure-slack):
subscribe to `app_mention` and the `message.*` events you want, and make sure the
bot has the `chat:write` scope — the Slackbot delivers replies with Slack's
streaming API, which requires it.

## Verify

```bash
kubectl get ingress -n centaur     # ADDRESS resolves to <name>.<tailnet>.ts.net
kubectl get pods -n tailscale      # operator + a ts-...-slackbot-... proxy, both Running
```

An unsigned POST should reach the Slackbot and be rejected *by the app* — proof
that TLS termination, routing, and the NetworkPolicy all work end to end:

```bash
curl -i -X POST https://<name>.<tailnet>.ts.net/api/webhooks/slack
# HTTP/2 401  {"ok":false,"error":"missing_signature_headers"}
```

A `401` from the Slackbot means success: `curl` validated the public Let's Encrypt
certificate without `-k`. Saving the Request URL in Slack should then verify green.

## Troubleshooting

- **Device appears but Funnel is off (tailnet-only):** the `funnel` nodeAttr is
  missing or targets `autogroup:member` instead of `tag:k8s`, or HTTPS certificates
  are not enabled for the tailnet.
- **Connection hangs or times out:** the Slackbot NetworkPolicy is still blocking
  the operator's proxy — confirm `tailscale` is in
  `networkPolicy.ingressControllerNamespaces` and that the namespace carries the
  `kubernetes.io/metadata.name: tailscale` label (automatic on Kubernetes ≥ 1.22).
