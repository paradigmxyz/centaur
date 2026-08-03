# PR CI status (fork)

PR: https://github.com/paradigmxyz/centaur/pull/1254

Workflow runs from the fork show `action_required`: upstream maintainers must
**Approve and run workflows** for first-time contributors. Fork authors cannot
call the approve API (`403 Must have admin rights`).

## What to ask a maintainer

> Please approve Actions for PR #1254 so CI / Console CI / Docs can run.

## Local validation already done (no CI required)

- Spaces import-boundary script
- Adapter unit tests
- HubSpot / Microsoft Graph mocked client tests
- Console allowlist transform proven via local console-worker image
  (`egress_allowlist_transform` returns the Spaces domain list)
