"""Shared sandbox configuration helpers."""

from __future__ import annotations

import os
import json
from urllib.parse import urlsplit

from api.deps import mint_sandbox_token
from api.sandbox.base import SandboxSession


def image() -> str:
    return os.getenv("AGENT_IMAGE", "centaur-agent:latest")


_HARNESS_STUB_KEYS = (
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "AMP_API_KEY",
    "GITHUB_TOKEN",
)

_SANDBOX_PASSTHROUGH_ENV_KEYS = (
    "CODEX_OTEL_ENVIRONMENT",
)

# Keep Claude Code deterministic in the pod while still allowing Centaur-owned
# OTel export from claude-app-wrapper.
_CLAUDE_HARDENING_ENV = (
    ("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY", "1"),
    ("CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL", "1"),
    ("CLAUDE_CODE_PROXY_RESOLVES_HOSTS", "1"),
    ("CLAUDE_CODE_CERT_STORE", "bundled,system"),
    ("DISABLE_ERROR_REPORTING", "1"),
    ("DISABLE_FEEDBACK_COMMAND", "1"),
    ("DISABLE_GROWTHBOOK", "1"),
    ("DISABLE_UPDATES", "1"),
)


def _set_env(env: list[str], name: str, value: str) -> None:
    prefix = f"{name}="
    entry = f"{name}={value}"
    for index, existing in enumerate(env):
        if existing.startswith(prefix):
            env[index] = entry
            return
    env.append(entry)


def _sandbox_extra_env() -> list[tuple[str, str]]:
    raw = (os.getenv("KUBERNETES_SANDBOX_EXTRA_ENV") or "").strip()
    if not raw:
        return []
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        return []
    if not isinstance(parsed, list):
        return []

    extra: list[tuple[str, str]] = []
    for item in parsed:
        if not isinstance(item, dict):
            continue
        name = str(item.get("name") or "").strip()
        if not name or "=" in name:
            continue
        value = item.get("value")
        extra.append((name, "" if value is None else str(value)))
    return extra


def amp_mode() -> str:
    return (os.getenv("AMP_MODE") or "deep").strip() or "deep"


def amp_thread_visibility() -> str | None:
    value = (os.getenv("AMP_THREAD_VISIBILITY") or "").strip()
    return value or None


def build_harness_cmd(engine: str, model: str | None = None) -> list[str]:
    """Build the container CMD for a given harness engine."""
    if engine == "amp":
        return ["amp-wrapper"]
    if engine == "codex":
        return ["codex-app-wrapper"]
    if engine == "claude-code":
        return ["claude-app-wrapper"]
    return ["sleep", "infinity"]


def container_env(
    thread_key: str,
    container_name: str,
    firewall_host: str,
    *,
    trace_id: str | None = None,
    resume_thread_id: str | None = None,
    pg_dsns: dict[str, str] | None = None,
) -> list[str]:
    """Build env vars for sandbox pods.

    ``firewall_host`` is the in-cluster service name of the per-sandbox
    iron-proxy. ``pg_dsns`` maps each ``pg_dsn`` secret name to the local
    DSN the sandbox should see (constructed by the backend to point at
    iron-proxy).
    """
    api_key = mint_sandbox_token(thread_key, container_name)
    api_url = os.getenv("AGENT_API_URL", "http://api:8000")

    env = [
        f"CENTAUR_API_URL={api_url}",
        f"CENTAUR_API_KEY={api_key}",
        f"CENTAUR_THREAD_KEY={thread_key}",
        f"CENTAUR_TRACE_ID={trace_id or ''}",
        f"AMP_MODE={amp_mode()}",
    ]
    visibility = amp_thread_visibility()
    if visibility:
        env.append(f"AMP_THREAD_VISIBILITY={visibility}")
    if resume_thread_id:
        env.append(f"AMP_CONTINUE_THREAD_ID={resume_thread_id}")

    no_proxy_hosts = ["localhost", "127.0.0.1", firewall_host]
    api_host = urlsplit(api_url).hostname
    if api_host:
        no_proxy_hosts.append(api_host)
    no_proxy = ",".join(dict.fromkeys(no_proxy_hosts))
    # Placeholder values for harness infra secrets. iron-proxy MITMs the
    # outbound TLS connection and rewrites these strings in auth headers
    # before they reach the real upstream.
    for key in _HARNESS_STUB_KEYS:
        env.append(f"{key}={key}")
    for key in _SANDBOX_PASSTHROUGH_ENV_KEYS:
        value = (os.getenv(key) or "").strip()
        if value:
            env.append(f"{key}={value}")
    for key, value in _CLAUDE_HARDENING_ENV:
        env.append(f"{key}={value}")
    env.extend(
        [
            f"FIREWALL_HOST={firewall_host}",
            f"HTTPS_PROXY=http://{firewall_host}:8080",
            f"HTTP_PROXY=http://{firewall_host}:8080",
            f"https_proxy=http://{firewall_host}:8080",
            f"http_proxy=http://{firewall_host}:8080",
            f"NO_PROXY={no_proxy}",
            f"no_proxy={no_proxy}",
            "NODE_EXTRA_CA_CERTS=/firewall-certs/ca-cert.pem",
            "REQUESTS_CA_BUNDLE=/firewall-certs/ca-cert.pem",
            "SSL_CERT_FILE=/firewall-certs/ca-cert.pem",
            "GIT_SSL_CAINFO=/firewall-certs/ca-cert.pem",
        ]
    )

    if pg_dsns:
        for name, dsn in pg_dsns.items():
            env.append(f"{name}={dsn}")

    for name, value in _sandbox_extra_env():
        _set_env(env, name, value)

    return env


def runtime_for_session(session: SandboxSession):
    from api.agent import _get_runtime

    return _get_runtime(session.sandbox_id)
