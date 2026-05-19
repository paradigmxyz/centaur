FROM python:3.11-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl openssh-client \
    ripgrep fd-find jq yq tree gettext-base \
    && ln -sf /usr/bin/fdfind /usr/local/bin/fd \
    && ARCH=$(dpkg --print-architecture) \
    && curl -fsSL -o /usr/local/bin/dbmate "https://github.com/amacneil/dbmate/releases/download/v2.32.0/dbmate-linux-${ARCH}" \
    && chmod +x /usr/local/bin/dbmate \
    && rm -rf /var/lib/apt/lists/*

COPY --from=ghcr.io/astral-sh/uv:0.7@sha256:629240833dd25d03949509fc01ceff56ae74f5e5f0fd264da634dd2f70e9cc70 /uv /uvx /usr/local/bin/

ENV UV_COMPILE_BYTECODE=1 UV_LINK_MODE=copy

WORKDIR /app

# 1. Install core deps (changes rarely — cached layer)
COPY services/api/pyproject.toml ./
COPY centaur_sdk/ centaur_sdk/
RUN sed -i \
        -e 's|path = "../../centaur_sdk"|path = "centaur_sdk"|' \
        pyproject.toml \
    && rm -f uv.lock
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --no-install-project --no-dev

# 2. Install tool deps from tools/**/pyproject.toml (changes rarely — cached layer)
RUN --mount=type=cache,target=/root/.cache/uv \
    --mount=type=bind,source=tools,target=/tmp/tools \
    python -c "\
import tomllib, pathlib; \
deps = set(); \
[deps.update(tomllib.load(open(p,'rb')).get('project',{}).get('dependencies',[])) \
 for root in ('/tmp/tools',) \
 for p in pathlib.Path(root).glob('**/pyproject.toml')]; \
open('/tmp/pd.txt','w').write('\n'.join(sorted(deps)))" \
    && uv pip install -r /tmp/pd.txt --quiet \
    && rm /tmp/pd.txt

# 3. Copy source (changes often — keep last for fast rebuilds)
COPY services/api/api/ api/
COPY services/api/db/ db/
COPY services/sandbox/SYSTEM_PROMPT.md services/sandbox/SYSTEM_PROMPT.md
COPY tools/ tools/
COPY workflows/ workflows/
RUN --mount=type=cache,target=/root/.cache/uv \
    uv sync --no-dev --inexact

COPY services/api/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 8000

ENTRYPOINT ["/entrypoint.sh"]
# --forwarded-allow-ips defaults to the loopback address so X-Forwarded-For is
# only honored from a co-located trusted proxy. Override FORWARDED_ALLOW_IPS
# with the real proxy IP(s) when fronted by a load balancer. Never set it to
# "*": that lets any peer spoof X-Forwarded-For: 127.0.0.1 and impersonate the
# localhost auth bypass in deps.verify_api_key.
CMD ["/app/.venv/bin/uvicorn", "api.app:app", "--host", "0.0.0.0", "--port", "8000", "--proxy-headers"]
