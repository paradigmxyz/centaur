# Centaur SDK

Lightweight toolkit for building Centaur-compatible tools.

## Install

```bash
# From git
pip install "centaur-sdk @ git+https://github.com/<owner>/<repo>.git#subdirectory=centaur_sdk"

# With HTTP backend support
pip install "centaur-sdk[http] @ git+https://github.com/<owner>/<repo>.git#subdirectory=centaur_sdk"
```

## Usage

### Secrets

```python
from centaur_sdk import secret

api_key = secret("MY_API_KEY")
```

In server mode, `secret()` returns stub values that the firewall replaces
with real credentials in-flight. In CLI mode, configure a backend first:

```python
from centaur_sdk.backends import configure, DotEnvBackend

configure(DotEnvBackend(".env"))
```
