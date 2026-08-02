# Spaces → Centaur adapter

Thin HTTP client for Centaur’s durable session API. This is the only Spaces
package that may encode Centaur request shapes (paths, headers, JSON bodies).

Everything else under `spaces/` must stay Centaur-agnostic: opaque session IDs,
HTTP tool clients, and product schema only.

## Usage

```python
from spaces_adapter import SessionClient

client = SessionClient(
    base_url="http://127.0.0.1:8080",
    token="<service-token>",
)
client.create_session(
    session_id="spaces-demo-1",
    harness_type="claudecode",
    metadata={"source": "spaces", "platform": "spaces"},
)
client.append_messages(
    session_id="spaces-demo-1",
    messages=[{"author": {"role": "user"}, "parts": [{"type": "text", "text": "Reply exactly PONG."}]}],
)
execution = client.execute(session_id="spaces-demo-1")
```

## Validation

```bash
uv run --project spaces/adapter python -m unittest discover -s spaces/adapter/tests -p 'test_*.py'
```
