# MPP

Registry-backed fallback for paid Machine Payments Protocol services.

```bash
mpp list
mpp search "weather"
mpp show <service-id>
mpp request <service-id> --method GET --path '/registered/:id' \
  --path-params '{"id":"value"}' --query '{"limit":10}'
mpp health
```

`request` accepts only a registry service ID and registered route. It rejects
arbitrary URLs, redirects, custom headers, host substitution, and path
traversal. GET routes are available by default; operator policy must explicitly
allow mutating methods.

The sandbox caches registry metadata at
`~/.cache/centaur/mpp/registry-v1.json`. Discovery may use a clearly marked
stale cache for up to 24 hours, while execution fails closed after that.
Payment credentials are created by the cluster-internal signer and never enter
the sandbox.
