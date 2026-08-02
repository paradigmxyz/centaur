# Spaces

Portable product layer for MagikDev’s internal agent platform, built on pinned
Centaur.

## Layout

| Path | Role |
|------|------|
| [`adapter/`](adapter/) | Only Centaur-aware code: thin session HTTP client |
| [`schema/`](schema/) | Future Spaces durable state (placeholder) |
| [`docs/phase0/`](docs/phase0/) | Phase 0 bring-up evidence and runbooks |
| [`docs/phase1/`](docs/phase1/) | OAuth/tools notes, Composio spike, Teams checklist |

## Import boundary

Nothing under `spaces/` may import Centaur packages (`centaur_sdk`, console
internals, etc.). Talk to Centaur through the adapter’s HTTP API shapes, or via
upstreamed console OAuth strategies under
`services/console/lib/oauth/providers/`.

```bash
.github/scripts/check-spaces-import-boundary.sh
uv run --project spaces/adapter python -m unittest discover -s spaces/adapter/tests -p 'test_*.py'
```
