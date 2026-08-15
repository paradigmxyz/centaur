# iron-proxy Guide

## Role

This directory wraps a pinned upstream `iron-proxy` image with Centaur's
startup contract and default unmanaged configuration. It is a security
boundary: sandboxes send placeholder credentials and the proxy applies only
the credential and request rules granted by the control plane.

## Invariants

- Keep the upstream image pinned by version and digest. Review upstream release
  notes and config compatibility before changing it.
- The CA key and certificate are required mounted inputs. Startup must fail
  closed when they are missing, and the private key must retain restrictive
  permissions.
- Managed mode takes its configuration from the control plane and must not mix
  in the baked local config. Unmanaged mode may seed the default config once
  without overwriting a mounted or generated file.
- Never bake credentials, provider tokens, private certificates, or deployment
  endpoints into the image or YAML.
- Header allowlisting and transforms are security policy. Add the narrowest
  header/host/path support required; do not use a broad exception to make one
  integration work.
- JSON logs must not contain request headers, credential values, proxy tokens,
  or upstream response bodies.

## Validation

Build from the repository root:

```bash
sh -n services/iron-proxy/entrypoint.sh
(cd services/api-rs && cargo test -p centaur-iron-proxy)
```

For entrypoint changes, exercise both managed and unmanaged modes and the
missing-CA failure path. For config or transform changes, deploy the local
stack and make a request from inside a sandbox through the proxy to a controlled
test upstream. Prove the intended header is injected, unrelated headers are
removed, and the sandbox still contains only placeholder material.
