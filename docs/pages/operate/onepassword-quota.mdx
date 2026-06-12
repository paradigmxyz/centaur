---
title: Recover from 1Password quota exhaustion
description: Runbook for sandbox failures caused by 1Password service-account throttling.
---

# Recover from 1Password quota exhaustion

Centaur's `onepassword` secret source reads `op://...` refs directly from the
1Password service-account API. That read budget is **account-wide**, not per
service account. Separate service accounts still help with identity separation
and audit trails, but they do not isolate quota.

:::warning[Shared budget]
If operator CLIs, background proxy churn, and the cluster all read through the
same 1Password account, they consume one shared rolling-window budget.
Creating another service account during an incident will not reset it.
:::

## Symptom signature

When the budget is exhausted, the failure shows up in two places:

| Surface | What you see |
|---------|---------------|
| iron-proxy logs | `secret_unavailable` and `rate limit exceeded` while resolving `op://...` refs. |
| Agent or harness boot | New runs fail early and crash-loop with `Invalid or missing API key` because the proxy cannot swap the placeholder credential for the real secret. |

Useful checks:

```bash
kubectl logs -n centaur -l centaur.ai/iron-proxy=true --since=15m | \
  rg 'secret_unavailable|rate limit exceeded'
```

```bash
kubectl get pods -n centaur -l centaur.ai/iron-proxy=true
```

## Immediate recovery

1. Stop the bleed.
   Pause any operator or CLI workflows that are repeatedly reading from
   1Password, and clean up stale per-sandbox proxies that no longer correspond
   to live work. REV-14's terminal-run garbage collection is the primary fix
   for this class of incident.
2. Wait for the rolling window to clear.
   Do not rotate to another service account expecting fresh quota; the limit is
   shared across the account.
3. Verify the error signature stops.
   Re-check the proxy logs and confirm a fresh sandbox can start without the
   `Invalid or missing API key` loop.

## Reduce steady-state load

Apply the levers in this order:

1. Eliminate background proxy churn.
   Orphaned sandboxes and proxies keep refreshing secrets even after the user
   work is over. Keep REV-14 deployed anywhere this incident matters.
2. Keep the proxy secret TTL long enough for steady state.
   The chart default is `ironProxy.secretTtl: 1h`, which cuts 1Password reads
   by 6x versus the old `10m` default. Override it only when you need faster
   propagation of secret changes.
3. Separate identities, but do not count on quota isolation.
   Use one service account for cluster secret resolution and another for
   operator reads if you want cleaner audit trails. Assume they still share one
   1Password budget.
4. Revisit the architecture as concurrency grows.
   If live sandbox count keeps climbing, prefer `onepassword-connect`, move
   cluster boot secrets off live 1Password reads, or evaluate the 1Password
   plan tier that changes service-account limits.

## Verify the fix

After changing TTLs or cleaning up leaked proxies, verify that request volume is
driven by live work rather than background churn:

1. Count live iron-proxy pods and compare that with active sandboxes.
2. Check recent proxy logs for the absence of `rate limit exceeded`.
3. Start one new sandbox and confirm its first provider call succeeds.

If the rate-limit signature returns while pod counts stay flat, the remaining
load is likely coming from operator or external readers rather than sandbox
lifecycle leaks.
