# CI status (Spaces fork only)

**Policy:** MagikDev does **not** contribute Spaces work to `paradigmxyz/centaur`.
Development, CI, and deploys stay on the MagikDev fork / private remote.

An accidental PR against upstream (`paradigmxyz/centaur#1254`) is not part of
this plan. Close it (or leave it idle). Do not ask upstream to approve Actions.
Validate on the laptop / CubePath VPS with local commands instead.

## Local validation (source of truth)

- Spaces import-boundary script
- Adapter unit tests
- HubSpot / Microsoft Graph mocked client tests
- Console allowlist transform + `just smoke` (Claude PONG) on kind **and** CubePath VPS
- Teamsbot unit tests + simulate
