"""Kubernetes sandbox backend — runs agent sandboxes as Pods."""

from __future__ import annotations

import asyncio
import contextlib
import hashlib
import json
import os
import re
import secrets as _secrets
import time
import uuid
from collections.abc import AsyncIterator, Mapping
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit, urlunsplit

from aiohttp import WSMsgType
from kubernetes_asyncio import client, config
from kubernetes_asyncio.config.config_exception import ConfigException
from kubernetes_asyncio.stream import WsApiClient
from kubernetes_asyncio.stream.ws_client import (
    ERROR_CHANNEL,
    STDERR_CHANNEL,
    STDIN_CHANNEL,
    STDOUT_CHANNEL,
)
import structlog

from api.broker_config import render_broker_yaml
from api.deps import mint_sandbox_token
from api.proxy_config import (
    assign_pg_listen_ports,
    core_pg_listen_port,
    render_proxy_yaml,
)
from api.sandbox.base import SandboxBackend, SandboxSession
from api.sandbox.config import (
    OBSERVABILITY_NO_PROXY_HOSTS,
    build_harness_cmd,
    container_env,
    image,
    runtime_for_session,
    sandbox_extra_env_map,
)
from api.sandbox.prompt_assembly import assemble_prompt
from api.tool_manager import PgDsnSecret, SecretDef

log = structlog.get_logger()

_READY_TIMEOUT_S = int(os.getenv("KUBERNETES_SANDBOX_READY_TIMEOUT_S", "60"))
_ATTACH_LOG_TAIL_LINES = int(os.getenv("KUBERNETES_ATTACH_LOG_TAIL_LINES", "200"))
_CONTAINER_NAME = "sandbox"
_AGENT_UID = 1001
_SANDBOX_OVERLAY_ROOT = "/home/agent/overlay"
_SANDBOX_OVERLAY_DIR = f"{_SANDBOX_OVERLAY_ROOT}/org"
_PROXY_LABEL = "centaur.ai/iron-proxy"
_API_PROXY_POD_NAME = "centaur-api-proxy"
_API_PROXY_SANDBOX_ID = "api"
# iron-token-broker resource names. The chart provisions the matching Service
# + NetworkPolicies under the same name; ensure_token_broker() applies the
# Deployment + ConfigMap so the broker config can be regenerated without a
# Helm upgrade. The label keeps the chart-side NetworkPolicy selector
# wired up.
_TOKEN_BROKER_LABEL = "centaur.ai/iron-token-broker"
def _get_rt(session: SandboxSession):
    return runtime_for_session(session)


def _repo_root() -> Path:
    for candidate in Path(__file__).resolve().parents:
        if (candidate / "services" / "sandbox" / "SYSTEM_PROMPT.md").is_file():
            return candidate
    raise FileNotFoundError("could not locate services/sandbox/SYSTEM_PROMPT.md")


def _overlay_root() -> Path | None:
    value = (os.getenv("CENTAUR_OVERLAY_DIR") or "").strip()
    if not value:
        return None
    path = Path(value)
    return path if path.exists() else None


def _namespace() -> str:
    configured = (
        os.getenv("KUBERNETES_NAMESPACE") or os.getenv("POD_NAMESPACE") or ""
    ).strip()
    if configured:
        return configured
    namespace_path = Path("/var/run/secrets/kubernetes.io/serviceaccount/namespace")
    if namespace_path.is_file():
        return namespace_path.read_text().strip()
    return "default"


def _image_pull_policy() -> str:
    return (os.getenv("KUBERNETES_AGENT_IMAGE_PULL_POLICY") or "IfNotPresent").strip()


def _runtime_class_name() -> str | None:
    value = (os.getenv("KUBERNETES_SANDBOX_RUNTIME_CLASS_NAME") or "").strip()
    return value or None


def _service_account_name() -> str | None:
    value = (os.getenv("KUBERNETES_SANDBOX_SERVICE_ACCOUNT_NAME") or "").strip()
    return value or None


def _state_volume_enabled() -> bool:
    value = (os.getenv("KUBERNETES_SANDBOX_STATE_VOLUME_ENABLED") or "").strip().lower()
    return value in {"1", "true", "yes", "on"}


def _env_int(name: str, default: int) -> int:
    raw = (os.getenv(name) or "").strip()
    return int(raw) if raw else default


def _proxy_port() -> int:
    return _env_int("KUBERNETES_IRON_PROXY_PORT", 8080)


def _proxy_management_port() -> int:
    return _env_int("KUBERNETES_IRON_PROXY_MANAGEMENT_PORT", 9092)


def _proxy_health_port() -> int:
    return _env_int("KUBERNETES_IRON_PROXY_HEALTH_PORT", 9090)


def _op_connect_app_name() -> str:
    return os.getenv("KUBERNETES_OP_CONNECT_APP_NAME", "onepassword-connect").strip()


def _op_connect_port() -> int:
    return _env_int("KUBERNETES_OP_CONNECT_PORT", 8080)


def _uses_op_connect_secret_source() -> bool:
    return (
        os.getenv("KUBERNETES_FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")
        == "onepassword-connect"
    )


def _proxy_image() -> str:
    return os.getenv("KUBERNETES_IRON_PROXY_IMAGE", "centaur-iron-proxy:latest")


def _tool_server_image() -> str | None:
    """Tool-server sidecar image.

    When set, sandbox Pods get a ``tool-server`` sidecar that exposes
    ``/tools/*`` on loopback. Sandboxes call ``http://localhost:<port>``
    instead of routing tool calls back to the API.
    """
    value = (os.getenv("KUBERNETES_TOOL_SERVER_IMAGE") or "").strip()
    return value or None


def _tool_server_image_pull_policy() -> str:
    return (
        os.getenv("KUBERNETES_TOOL_SERVER_IMAGE_PULL_POLICY") or _image_pull_policy()
    ).strip()


def _tool_server_port() -> int:
    return _env_int("KUBERNETES_TOOL_SERVER_PORT", 8001)


def _workflow_run_image() -> str:
    """Image used for per-run workflow execution pods.

    Defaults to ``KUBERNETES_WORKFLOW_RUN_IMAGE`` (Helm injects the api image
    by default) and falls back to ``centaur-api:latest`` for local runs.
    """
    return os.getenv("KUBERNETES_WORKFLOW_RUN_IMAGE", "centaur-api:latest")


def _workflow_run_image_pull_policy() -> str:
    return (
        os.getenv("KUBERNETES_WORKFLOW_RUN_IMAGE_PULL_POLICY") or _image_pull_policy()
    ).strip()


def _workflow_run_pod_name(run_id: str) -> str:
    return _resource_name("centaur-centaur-workflow-run", run_id)


def _tool_server_tool_dirs() -> str:
    """TOOL_DIRS the sidecar uses.

    The API fully controls both of the sidecar's mounts, so it constructs the
    path directly rather than inheriting (and rewriting) its own ``TOOL_DIRS``.
    Base tools live at ``/app/tools`` in the shared image. The overlay, when
    present, is mounted at ``_SANDBOX_OVERLAY_DIR`` — not the API's overlay
    mount — so its tools are at ``<_SANDBOX_OVERLAY_DIR>/tools``. An explicit
    ``KUBERNETES_TOOL_SERVER_TOOL_DIRS`` still wins as an escape hatch.
    """
    value = (os.getenv("KUBERNETES_TOOL_SERVER_TOOL_DIRS") or "").strip()
    if value:
        return value
    dirs = ["/app/tools"]
    if _overlay_image():
        dirs.append(f"{_SANDBOX_OVERLAY_DIR}/tools")
    return ":".join(dirs)


def _token_broker_name() -> str:
    return (os.getenv("KUBERNETES_TOKEN_BROKER_NAME") or "").strip()


def _token_broker_url() -> str:
    return (os.getenv("KUBERNETES_TOKEN_BROKER_URL") or "").strip()


def _token_broker_enabled() -> bool:
    """Whether the iron-token-broker is deployed in this cluster.

    Gated on the chart-set ``KUBERNETES_TOKEN_BROKER_URL`` (present only when
    ``tokenBroker.enabled=true``). Routing through the broker is opt-in per
    secret via the ``brokered_token`` type; this flag just controls whether
    iron-proxy receives broker env and whether the API reconciles the broker
    ConfigMap.
    """
    return bool(_token_broker_url())


def _token_broker_configmap_name() -> str:
    name = _token_broker_name()
    return f"{name}-config" if name else ""


def _proxy_image_pull_policy() -> str:
    return (
        os.getenv("KUBERNETES_IRON_PROXY_IMAGE_PULL_POLICY") or _image_pull_policy()
    ).strip()


def _secret_env_name() -> str:
    value = (os.getenv("KUBERNETES_SECRET_ENV_NAME") or "").strip()
    if not value:
        raise ValueError("KUBERNETES_SECRET_ENV_NAME is required for per-sandbox proxy")
    return value


def _bootstrap_secret_name() -> str:
    return (os.getenv("KUBERNETES_BOOTSTRAP_SECRET_NAME") or "").strip()


def _secret_env_key(name: str) -> str:
    return f"{os.getenv('KUBERNETES_SECRET_ENV_PREFIX', '')}{name}"


def _proxy_iron_env(
    secret_name: str,
    pg_secrets: list[tuple[PgDsnSecret, str]],
    core: dict[str, Any] | None = None,
) -> list[dict[str, Any]]:
    """Env block for the iron-proxy container.

    iron-proxy resolves the upstream DSN itself from its configured source
    (env / 1Password). The API only injects the per-listener proxy-side
    password env var so the sandbox can authenticate. ``pg_secrets`` carries
    ``(secret, proxy_password)`` for each declared ``pg_dsn``.
    """
    env: list[dict[str, Any]] = [
        {
            "name": "IRON_MANAGEMENT_API_KEY",
            "valueFrom": {
                "secretKeyRef": {
                    "name": secret_name,
                    "key": _secret_env_key("IRON_MANAGEMENT_API_KEY"),
                }
            },
        }
    ]
    if _uses_op_connect_secret_source():
        connect_host = os.getenv("KUBERNETES_OP_CONNECT_HOST", "").strip()
        if connect_host:
            env.append({"name": "OP_CONNECT_HOST", "value": connect_host})
        env.append(
            {
                "name": "OP_CONNECT_TOKEN",
                "valueFrom": {
                    "secretKeyRef": {
                        "name": secret_name,
                        "key": _secret_env_key("OP_CONNECT_TOKEN"),
                    }
                },
            }
        )
    broker_url = _token_broker_url()
    if broker_url:
        env.append({"name": "IRON_BROKER_URL", "value": broker_url})
        # The broker's bearer token lives alongside other infra secrets in
        # the centaur-infra-env Secret. Mirror IRON_MANAGEMENT_API_KEY's
        # shape so the operator only needs to bootstrap one secret manifest.
        env.append(
            {
                "name": "IRON_BROKER_TOKEN",
                "valueFrom": {
                    "secretKeyRef": {
                        "name": secret_name,
                        "key": _secret_env_key("IRON_BROKER_TOKEN"),
                    }
                },
            }
        )
    for secret, proxy_password in pg_secrets:
        env.append(
            {"name": f"PG_PROXY_PASSWORD_{secret.name}", "value": proxy_password}
        )
    if core is not None:
        env.append({"name": core["password_env"], "value": core["password"]})
    return env


def _build_proxied_pg_url(host: str, port: int, password: str, database: str) -> str:
    """Build a local postgres URL pointing at iron-proxy's listener.

    iron-proxy forwards the client's startup-packet database name to the
    upstream, so the dbname declared in the tool's pyproject must match the
    upstream's database name.
    """
    netloc = f"app_user:{password}@{host}:{port}"
    return urlunsplit(("postgresql", netloc, f"/{database}", "", ""))


_CORE_PG_PASSWORD_ENV = "PG_PROXY_PASSWORD_CENTAUR_CORE"


def _core_db_name() -> str:
    """Database name of the core Centaur DB, parsed from the API's own DSN.

    iron-proxy forwards the client's startup-packet database to the upstream,
    so the sidecar's proxied DSN must declare the same dbname.
    """
    from api.config import settings

    return urlsplit(settings.database_url).path.lstrip("/") or "centaur"


def _build_core_pg(
    firewall_host: str, pg_listen_ports: dict[str, int]
) -> dict[str, Any]:
    """Wiring for the per-sandbox proxy's core-DB listener (sidecar use only).

    The tool-server sidecar reaches the core DB through the proxy because the
    sandbox is denied direct Postgres egress. Returns the fields needed to
    render the listener, inject the proxy-side password, open the sidecar's
    pool, and allow egress to the listener port. Never injected into the agent
    container, so no DB credential lives in the sandbox.
    """
    port = core_pg_listen_port(pg_listen_ports)
    password = _secrets.token_urlsafe(24)
    return {
        "port": port,
        "password": password,
        "password_env": _CORE_PG_PASSWORD_ENV,
        "dsn_env_var": _secret_env_key("DATABASE_URL"),
        "dsn": _build_proxied_pg_url(firewall_host, port, password, _core_db_name()),
    }


def _api_pod_match_labels() -> dict[str, str]:
    return _parse_match_labels(
        os.getenv(
            "KUBERNETES_API_POD_LABEL_SELECTOR", "app.kubernetes.io/component=api"
        )
    )


def _parse_match_labels(raw: str) -> dict[str, str]:
    result: dict[str, str] = {}
    for item in raw.split(","):
        item = item.strip()
        if not item:
            continue
        key, sep, value = item.partition("=")
        if not sep:
            raise ValueError(
                f"invalid label selector item {item!r}; expected key=value"
            )
        result[key.strip()] = value.strip()
    return result


def _repos_path() -> str | None:
    value = (os.getenv("REPOS_PATH") or "").strip()
    return value or None


def _overlay_image() -> str | None:
    value = (os.getenv("CENTAUR_OVERLAY_IMAGE") or "").strip()
    return value or None


def _overlay_image_pull_policy() -> str:
    value = (os.getenv("CENTAUR_OVERLAY_IMAGE_PULL_POLICY") or "").strip()
    return value or _image_pull_policy()


def _overlay_image_source_path() -> str:
    value = (os.getenv("CENTAUR_OVERLAY_IMAGE_SOURCE_PATH") or "/overlay").strip()
    return value or "/overlay"


def _image_pull_secrets() -> list[dict[str, str]]:
    raw = (os.getenv("KUBERNETES_SANDBOX_IMAGE_PULL_SECRETS") or "").strip()
    if not raw:
        return []
    return [{"name": item.strip()} for item in raw.split(",") if item.strip()]


def _firewall_ca_secret_name() -> str:
    value = (os.getenv("KUBERNETES_FIREWALL_CA_SECRET_NAME") or "").strip()
    if not value:
        raise ValueError(
            "KUBERNETES_FIREWALL_CA_SECRET_NAME is required for kubernetes backend"
        )
    return value


def _firewall_ca_key_secret_name() -> str:
    value = (os.getenv("KUBERNETES_FIREWALL_CA_KEY_SECRET_NAME") or "").strip()
    if not value:
        raise ValueError(
            "KUBERNETES_FIREWALL_CA_KEY_SECRET_NAME is required for per-sandbox proxy"
        )
    return value


def _build_tool_server_container(
    *,
    thread_key: str,
    container_name: str,
    firewall_host: str,
    api_url: str,
    overlay_mount: str | None,
    database_url: str,
) -> dict[str, Any]:
    """Build the tool-server sidecar container spec.

    The sidecar listens on loopback inside the sandbox Pod and routes its own
    HTTP egress through the per-sandbox iron-proxy. ``database_url`` points at
    the proxy's core-DB listener (not the raw DSN) so the pool respects the
    sandbox's NetworkPolicy; the real credentials stay in the proxy pod.
    Caller is responsible for only invoking this when ``_tool_server_image()``
    is set.

    The sidecar runs tool code that calls back into the API (e.g. the slack
    tool offloading a downloaded file to ``/agent/attachments/upload``), so it
    needs its own ``CENTAUR_API_KEY``. Mint a sandbox token scoped to this
    thread, mirroring the agent container; without it the callback is
    unauthenticated and the API rejects it with 401.
    """
    image_ref = _tool_server_image()
    if not image_ref:
        raise RuntimeError("_build_tool_server_container called without an image")

    secret_name = _secret_env_name()
    proxy_url = f"http://{firewall_host}:{_proxy_port()}"
    api_host = urlsplit(api_url).hostname or ""
    no_proxy_hosts = [
        "localhost",
        "127.0.0.1",
        firewall_host,
        *OBSERVABILITY_NO_PROXY_HOSTS,
    ]
    if api_host:
        no_proxy_hosts.append(api_host)
    no_proxy = ",".join(dict.fromkeys(no_proxy_hosts))

    env: list[dict[str, Any]] = [
        {"name": "DATABASE_URL", "value": database_url},
        {
            "name": "SANDBOX_SIGNING_KEY",
            "valueFrom": {
                "secretKeyRef": {
                    "name": secret_name,
                    "key": _secret_env_key("SANDBOX_SIGNING_KEY"),
                }
            },
        },
        {"name": "HTTPS_PROXY", "value": proxy_url},
        {"name": "HTTP_PROXY", "value": proxy_url},
        {"name": "https_proxy", "value": proxy_url},
        {"name": "http_proxy", "value": proxy_url},
        {"name": "NO_PROXY", "value": no_proxy},
        {"name": "no_proxy", "value": no_proxy},
        {"name": "REQUESTS_CA_BUNDLE", "value": "/firewall-certs/ca-cert.pem"},
        {"name": "SSL_CERT_FILE", "value": "/firewall-certs/ca-cert.pem"},
        {"name": "NODE_EXTRA_CA_CERTS", "value": "/firewall-certs/ca-cert.pem"},
        {"name": "CENTAUR_API_URL", "value": api_url},
        {"name": "CENTAUR_API_KEY", "value": mint_sandbox_token(thread_key, container_name)},
        {"name": "TOOL_DIRS", "value": _tool_server_tool_dirs()},
        {"name": "PLUGIN_WATCHER_ENABLED", "value": "0"},
    ]
    _apply_tool_server_extra_env(env, no_proxy)

    volume_mounts: list[dict[str, Any]] = [
        {
            "name": "firewall-ca",
            "mountPath": "/firewall-certs",
            "readOnly": True,
        },
    ]
    if overlay_mount:
        volume_mounts.append(
            {
                "name": "overlay-root",
                "mountPath": overlay_mount,
                "readOnly": True,
            }
        )

    port = _tool_server_port()
    # Bind the listener on 0.0.0.0 inside the pod's network namespace. The
    # sandbox container reaches it via 127.0.0.1 (shared loopback within the
    # pod); kubelet probes reach it via the pod IP. Listening only on
    # 127.0.0.1 would block liveness probes — the kubelet runs on the node
    # and can't see the container's loopback interface — and the resulting
    # probe failures would kill the sidecar.
    return {
        "name": "tool-server",
        "image": image_ref,
        "imagePullPolicy": _tool_server_image_pull_policy(),
        # Same image as the API; different uvicorn target.
        "command": ["/app/.venv/bin/uvicorn"],
        "args": [
            "api.tool_server_app:app",
            "--host",
            "0.0.0.0",
            "--port",
            str(port),
        ],
        "env": env,
        "ports": [{"containerPort": port, "name": "tools"}],
        "readinessProbe": {
            "httpGet": {"path": "/healthz", "port": port},
            "periodSeconds": 5,
            "failureThreshold": 30,
        },
        "livenessProbe": {
            "httpGet": {"path": "/healthz", "port": port},
            "periodSeconds": 30,
            "failureThreshold": 5,
        },
        "securityContext": {
            "allowPrivilegeEscalation": False,
            "capabilities": {"drop": ["ALL"]},
            "runAsGroup": _AGENT_UID,
            "runAsNonRoot": True,
            "runAsUser": _AGENT_UID,
            "seccompProfile": {"type": "RuntimeDefault"},
        },
        "volumeMounts": volume_mounts,
    }


def _apply_tool_server_extra_env(env: list[dict[str, Any]], computed_no_proxy: str) -> None:
    """Let the sidecar see operator sandbox env without breaking its wiring."""
    pinned = {
        "DATABASE_URL",
        "SANDBOX_SIGNING_KEY",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "REQUESTS_CA_BUNDLE",
        "SSL_CERT_FILE",
        "NODE_EXTRA_CA_CERTS",
        "CENTAUR_API_URL",
        "CENTAUR_API_KEY",
        "TOOL_DIRS",
        "PLUGIN_WATCHER_ENABLED",
    }
    no_proxy_keys = {"NO_PROXY", "no_proxy"}
    for name, value in sandbox_extra_env_map().items():
        if name in pinned:
            log.warning("tool_server_extra_env_ignored_pinned_var", key=name)
            continue
        if name in no_proxy_keys:
            value = _merge_csv_env(computed_no_proxy, value)
        _upsert_env_value(env, name, value)


def _upsert_env_value(env: list[dict[str, Any]], name: str, value: str) -> None:
    for item in env:
        if item.get("name") == name:
            item["value"] = value
            item.pop("valueFrom", None)
            return
    env.append({"name": name, "value": value})


def _merge_csv_env(base: str, extra: str) -> str:
    values = [item.strip() for item in base.split(",") if item.strip()]
    values.extend(item.strip() for item in extra.split(",") if item.strip())
    return ",".join(dict.fromkeys(values))


def _resource_name(prefix: str, raw: str, *, max_length: int = 63) -> str:
    digest = hashlib.sha1(raw.encode("utf-8")).hexdigest()[:10]
    normalized = re.sub(r"[^a-z0-9-]+", "-", raw.lower()).strip("-") or "session"
    budget = max_length - len(prefix) - len(digest) - 2
    trimmed = normalized[: max(budget, 1)].strip("-") or "session"
    return f"{prefix}-{trimmed}-{digest}"


def _prompt_secret_name(pod_name: str) -> str:
    base = pod_name[: 63 - len("-cfg")].rstrip("-") or "centaur-centaur-sandbox"
    return f"{base}-cfg"


def _proxy_pod_name(sandbox_id: str) -> str:
    if sandbox_id == _API_PROXY_SANDBOX_ID:
        return _API_PROXY_POD_NAME
    return _resource_name("centaur-centaur-proxy", sandbox_id)


def _new_proxy_pod_name(sandbox_id: str) -> str:
    if sandbox_id == _API_PROXY_SANDBOX_ID:
        return _API_PROXY_POD_NAME
    return _resource_name(
        "centaur-centaur-proxy",
        f"{sandbox_id}:{uuid.uuid4().hex[:8]}",
    )


def _proxy_service_name(sandbox_id: str) -> str:
    if sandbox_id == _API_PROXY_SANDBOX_ID:
        return _API_PROXY_POD_NAME
    return _resource_name("centaur-centaur-proxy", sandbox_id)


def _proxy_configmap_name(sandbox_id: str) -> str:
    return f"{_proxy_pod_name(sandbox_id)}-config"


def _sandbox_egress_policy_name(sandbox_id: str) -> str:
    return _resource_name("centaur-centaur-sbx-egress", sandbox_id)


def _proxy_policy_name(sandbox_id: str) -> str:
    return _resource_name("centaur-centaur-proxy-net", sandbox_id)


def _ensure_kubernetes_env() -> None:
    if not (os.getenv("AGENT_API_URL") or "").strip():
        raise ValueError("AGENT_API_URL is required for kubernetes backend")


def _pod_resources() -> dict[str, Any]:
    limits: dict[str, str] = {}
    cpu_limit = os.environ.get("KUBERNETES_SANDBOX_CPU_LIMIT")
    memory_limit = os.environ.get("KUBERNETES_SANDBOX_MEMORY_LIMIT")
    if cpu_limit is None:
        limits["cpu"] = "2"
    elif cpu_limit.strip():
        limits["cpu"] = cpu_limit.strip()
    if memory_limit is None:
        limits["memory"] = "4Gi"
    elif memory_limit.strip():
        limits["memory"] = memory_limit.strip()

    requests: dict[str, str] = {}
    cpu_request = (os.getenv("KUBERNETES_SANDBOX_CPU_REQUEST") or "").strip()
    memory_request = (os.getenv("KUBERNETES_SANDBOX_MEMORY_REQUEST") or "").strip()
    if cpu_request:
        requests["cpu"] = cpu_request
    if memory_request:
        requests["memory"] = memory_request

    resources: dict[str, Any] = {}
    if limits:
        resources["limits"] = limits
    if requests:
        resources["requests"] = requests
    return resources


def _prompt_bundle(persona: str | None) -> str:
    base_prompt = (
        _repo_root() / "services" / "sandbox" / "SYSTEM_PROMPT.md"
    ).read_text()
    overlay_root = _overlay_root()
    overlay_prompt = (
        overlay_root / "services" / "sandbox" / "SYSTEM_PROMPT.md"
        if overlay_root is not None
        else None
    )
    persona_info = None
    if persona:
        from api.app import get_tool_manager

        persona_info = get_tool_manager().get_persona(persona)
        if persona_info is None:
            log.warning("persona_not_found_for_kubernetes_backend", persona=persona)
    return assemble_prompt(
        persona,
        base_prompt=base_prompt,
        overlay_prompt_path=overlay_prompt,
        persona_info=persona_info,
        api_overlay_dir=overlay_root,
        sandbox_overlay_dir=_SANDBOX_OVERLAY_DIR if _overlay_image() else None,
    )


def _parse_ws_frame(data: bytes | str) -> tuple[int, str]:
    if isinstance(data, bytes):
        return data[0], data[1:].decode("utf-8", errors="replace")
    return ord(data[0]), data[1:]


def _disable_proxy_env(api_client: client.ApiClient | WsApiClient) -> None:
    # The API process routes outbound HTTPS through the firewall, but the
    # in-cluster Kubernetes client must talk directly to the apiserver.
    api_client.rest_client.pool_manager._trust_env = False


class KubernetesExecutorBackend(SandboxBackend):
    """Runs agent sandboxes as Kubernetes Pods."""

    def __init__(self) -> None:
        self._core: client.CoreV1Api | None = None
        self._networking: client.NetworkingV1Api | None = None
        self._apps: client.AppsV1Api | None = None
        self._ws_api_client: WsApiClient | None = None
        self._ws_core: client.CoreV1Api | None = None
        self._lock = asyncio.Lock()

    @property
    def name(self) -> str:
        return "kubernetes"

    @property
    def supports_warm_pool(self) -> bool:
        return True

    async def _ensure_clients(self) -> None:
        ready = (
            self._core is not None
            and self._networking is not None
            and self._ws_api_client is not None
            and self._ws_core is not None
        )
        if ready:
            return
        async with self._lock:
            ready = (
                self._core is not None
                and self._networking is not None
                and self._ws_api_client is not None
                and self._ws_core is not None
            )
            if ready:
                return
            kubeconfig = (os.getenv("KUBERNETES_KUBECONFIG") or "").strip()
            if kubeconfig:
                await config.load_kube_config(config_file=kubeconfig)
            else:
                try:
                    config.load_incluster_config()
                except ConfigException:
                    await config.load_kube_config()
            core_api_client = client.ApiClient(
                configuration=client.Configuration.get_default_copy()
            )
            _disable_proxy_env(core_api_client)
            self._core = client.CoreV1Api(api_client=core_api_client)
            self._networking = client.NetworkingV1Api(api_client=core_api_client)
            self._apps = client.AppsV1Api(api_client=core_api_client)

            self._ws_api_client = WsApiClient(
                configuration=client.Configuration.get_default_copy(),
                heartbeat=30,
            )
            _disable_proxy_env(self._ws_api_client)
            self._ws_core = client.CoreV1Api(api_client=self._ws_api_client)

    def _core_api(self) -> client.CoreV1Api:
        if self._core is None:
            raise RuntimeError("kubernetes client not initialized")
        return self._core

    def _networking_api(self) -> client.NetworkingV1Api:
        if self._networking is None:
            raise RuntimeError("kubernetes client not initialized")
        return self._networking

    def _apps_api(self) -> client.AppsV1Api:
        if self._apps is None:
            raise RuntimeError("kubernetes client not initialized")
        return self._apps

    def _ws_core_api(self) -> client.CoreV1Api:
        if self._ws_core is None:
            raise RuntimeError("kubernetes websocket client not initialized")
        return self._ws_core

    def _ws_api(self) -> WsApiClient:
        if self._ws_api_client is None:
            raise RuntimeError("kubernetes websocket client not initialized")
        return self._ws_api_client

    @staticmethod
    def _is_not_found(exc: Exception) -> bool:
        return getattr(exc, "status", None) == 404

    async def _delete_prompt_secret(self, secret_name: str) -> None:
        try:
            await self._core_api().delete_namespaced_secret(secret_name, _namespace())
        except Exception as exc:
            if not self._is_not_found(exc):
                raise

    async def _delete_pod(self, pod_name: str) -> None:
        try:
            await self._core_api().delete_namespaced_pod(
                pod_name,
                _namespace(),
                grace_period_seconds=5,
            )
        except Exception as exc:
            if not self._is_not_found(exc):
                raise

    async def _delete_service(self, service_name: str) -> None:
        try:
            await self._core_api().delete_namespaced_service(service_name, _namespace())
        except Exception as exc:
            if not self._is_not_found(exc):
                raise

    async def _delete_network_policy(self, policy_name: str) -> None:
        try:
            await self._networking_api().delete_namespaced_network_policy(
                policy_name,
                _namespace(),
            )
        except Exception as exc:
            if not self._is_not_found(exc):
                raise

    async def _delete_configmap(self, name: str) -> None:
        try:
            await self._core_api().delete_namespaced_config_map(name, _namespace())
        except Exception as exc:
            if not self._is_not_found(exc):
                raise

    async def _delete_proxy_resources(self, sandbox_id: str) -> None:
        await self._delete_pod(_proxy_pod_name(sandbox_id))
        await self._delete_proxy_pods_for_sandbox(sandbox_id)
        await self._delete_service(_proxy_service_name(sandbox_id))
        await self._delete_configmap(_proxy_configmap_name(sandbox_id))
        await self._delete_network_policy(_sandbox_egress_policy_name(sandbox_id))
        await self._delete_network_policy(_proxy_policy_name(sandbox_id))

    async def _delete_proxy_pods_for_sandbox(self, sandbox_id: str) -> None:
        try:
            pods = await self._core_api().list_namespaced_pod(
                _namespace(),
                label_selector=f"{_PROXY_LABEL}=true,centaur.ai/sandbox-id={sandbox_id}",
            )
        except Exception as exc:
            if not self._is_not_found(exc):
                raise
            return
        for item in getattr(pods, "items", []) or []:
            metadata = getattr(item, "metadata", None)
            pod_name = getattr(metadata, "name", "") if metadata is not None else ""
            if pod_name:
                await self._delete_pod(pod_name)

    def _configure_workload_volumes(
        self,
        volume_mounts: list[dict[str, Any]],
        volumes: list[dict[str, Any]],
    ) -> None:
        if _state_volume_enabled():
            raise ValueError(
                "KUBERNETES_SANDBOX_STATE_VOLUME_ENABLED requires "
                "KUBERNETES_SANDBOX_CONTROLLER=agent-sandbox"
            )

    async def _delete_existing_workload(self, pod_name: str) -> None:
        await self._delete_pod(pod_name)

    async def _create_workload(self, pod_spec: dict[str, Any]) -> None:
        await self._core_api().create_namespaced_pod(_namespace(), pod_spec)

    async def _cleanup_workload_after_create_error(self, pod_name: str) -> None:
        await self._delete_pod(pod_name)

    def _collect_secrets(self) -> list[SecretDef]:
        from api.app import get_tool_manager

        return get_tool_manager().collect_secrets()

    def _secrets_for_sandbox(
        self, engine: str, auth_modes: Mapping[str, str]
    ) -> list[SecretDef]:
        from api.app import get_tool_manager

        return get_tool_manager().secrets_for_sandbox(engine, auth_modes)

    def _resolved_pg_secrets(
        self, secrets: list[SecretDef]
    ) -> list[tuple[PgDsnSecret, str]]:
        """For every distinct ``PgDsnSecret`` return ``(secret, proxy_password)``.

        The random proxy password is shared between iron-proxy (via env) and
        the sandbox (via the constructed local DSN). iron-proxy resolves the
        upstream DSN itself from the source declared in proxy.yaml.
        """
        out: dict[str, tuple[PgDsnSecret, str]] = {}
        for secret in secrets:
            if not isinstance(secret, PgDsnSecret):
                continue
            if secret.name in out:
                continue
            out[secret.name] = (secret, _secrets.token_urlsafe(24))
        return [out[name] for name in sorted(out)]

    async def _create_proxy_configmap(
        self,
        sandbox_id: str,
        secrets: list[SecretDef],
        pg_listen_ports: dict[str, int],
        core: dict[str, Any] | None = None,
    ) -> None:
        core_pg = (
            {k: core[k] for k in ("port", "dsn_env_var", "password_env")}
            if core is not None
            else None
        )
        rendered = render_proxy_yaml(
            secrets,
            base_config=None,
            pg_listen_ports=pg_listen_ports,
            core_pg=core_pg,
        )
        name = _proxy_configmap_name(sandbox_id)
        await self._delete_configmap(name)
        await self._core_api().create_namespaced_config_map(
            _namespace(),
            {
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": {
                    "name": name,
                    "labels": {
                        _PROXY_LABEL: "true",
                        "centaur.ai/sandbox-id": sandbox_id,
                    },
                },
                "data": {"proxy.yaml": rendered},
            },
        )

    async def _create_prompt_secret(
        self, secret_name: str, persona: str | None
    ) -> None:
        await self._delete_prompt_secret(secret_name)
        await self._core_api().create_namespaced_secret(
            _namespace(),
            {
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {
                    "name": secret_name,
                    "labels": {
                        "centaur.ai/managed": "true",
                    },
                },
                "type": "Opaque",
                "stringData": {
                    "AGENTS_BASE.md": _prompt_bundle(persona),
                },
            },
        )

    async def _create_proxy_service(
        self,
        sandbox_id: str,
        pg_listen_ports: dict[str, int],
        core: dict[str, Any] | None = None,
    ) -> None:
        service_name = _proxy_service_name(sandbox_id)
        await self._delete_service(service_name)
        ports: list[dict[str, Any]] = [
            {
                "name": "proxy",
                "port": _proxy_port(),
                "targetPort": _proxy_port(),
                "protocol": "TCP",
            }
        ]
        for name, port in sorted(pg_listen_ports.items(), key=lambda item: item[1]):
            ports.append(
                {
                    "name": f"pg-{name[:11].lower().replace('_', '-')}",
                    "port": port,
                    "targetPort": port,
                    "protocol": "TCP",
                }
            )
        if core is not None:
            ports.append(
                {
                    "name": "pg-core",
                    "port": core["port"],
                    "targetPort": core["port"],
                    "protocol": "TCP",
                }
            )
        await self._core_api().create_namespaced_service(
            _namespace(),
            {
                "apiVersion": "v1",
                "kind": "Service",
                "metadata": {
                    "name": service_name,
                    "labels": {
                        _PROXY_LABEL: "true",
                        "centaur.ai/sandbox-id": sandbox_id,
                    },
                },
                "spec": {
                    "selector": {
                        _PROXY_LABEL: "true",
                        "centaur.ai/sandbox-id": sandbox_id,
                    },
                    "ports": ports,
                },
            },
        )

    async def _create_proxy_network_policies(
        self,
        sandbox_id: str,
        pg_listen_ports: dict[str, int],
        core_port: int | None = None,
    ) -> None:
        await self._delete_network_policy(_sandbox_egress_policy_name(sandbox_id))
        await self._delete_network_policy(_proxy_policy_name(sandbox_id))

        sandbox_to_proxy_ports = [{"protocol": "TCP", "port": _proxy_port()}]
        for _, port in sorted(pg_listen_ports.items(), key=lambda item: item[1]):
            sandbox_to_proxy_ports.append({"protocol": "TCP", "port": port})
        if core_port is not None:
            # Lets the tool-server sidecar reach the proxy's core-DB listener.
            sandbox_to_proxy_ports.append({"protocol": "TCP", "port": core_port})

        proxy_egress = [
            {
                "to": [{"podSelector": {"matchLabels": _api_pod_match_labels()}}],
                "ports": [{"protocol": "TCP", "port": 8000}],
            },
            {
                "ports": [{"protocol": "TCP", "port": 443}],
            },
            {
                "ports": [{"protocol": "TCP", "port": 5432}],
            },
        ]
        if _uses_op_connect_secret_source():
            proxy_egress.append(
                {
                    "to": [
                        {
                            "podSelector": {
                                "matchLabels": {"app": _op_connect_app_name()}
                            }
                        }
                    ],
                    "ports": [{"protocol": "TCP", "port": _op_connect_port()}],
                }
            )

        await self._networking_api().create_namespaced_network_policy(
            _namespace(),
            {
                "apiVersion": "networking.k8s.io/v1",
                "kind": "NetworkPolicy",
                "metadata": {
                    "name": _sandbox_egress_policy_name(sandbox_id),
                    "labels": {
                        "centaur.ai/sandbox-id": sandbox_id,
                    },
                },
                "spec": {
                    "podSelector": {
                        "matchLabels": {
                            "centaur.ai/managed": "true",
                            "centaur.ai/sandbox-id": sandbox_id,
                        }
                    },
                    "policyTypes": ["Egress"],
                    "egress": [
                        {
                            "to": [
                                {
                                    "podSelector": {
                                        "matchLabels": _api_pod_match_labels()
                                    }
                                }
                            ],
                            "ports": [{"protocol": "TCP", "port": 8000}],
                        },
                        {
                            "to": [
                                {
                                    "podSelector": {
                                        "matchLabels": {
                                            _PROXY_LABEL: "true",
                                            "centaur.ai/sandbox-id": sandbox_id,
                                        }
                                    }
                                }
                            ],
                            "ports": sandbox_to_proxy_ports,
                        },
                    ],
                },
            },
        )
        await self._networking_api().create_namespaced_network_policy(
            _namespace(),
            {
                "apiVersion": "networking.k8s.io/v1",
                "kind": "NetworkPolicy",
                "metadata": {
                    "name": _proxy_policy_name(sandbox_id),
                    "labels": {
                        "centaur.ai/sandbox-id": sandbox_id,
                    },
                },
                "spec": {
                    "podSelector": {
                        "matchLabels": {
                            _PROXY_LABEL: "true",
                            "centaur.ai/sandbox-id": sandbox_id,
                        }
                    },
                    "policyTypes": ["Ingress", "Egress"],
                    "ingress": [
                        {
                            "from": [
                                {
                                    "podSelector": {
                                        "matchLabels": {
                                            "centaur.ai/managed": "true",
                                            "centaur.ai/sandbox-id": sandbox_id,
                                        }
                                    }
                                }
                            ],
                            "ports": sandbox_to_proxy_ports,
                        }
                    ],
                    "egress": proxy_egress,
                },
            },
        )

    def _build_proxy_pod_spec(
        self,
        sandbox_id: str,
        pg_secrets: list[tuple[PgDsnSecret, str]],
        pg_listen_ports: dict[str, int],
        *,
        restart_policy: str,
        core: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Return the pod.spec dict shared by the sandbox bare Pod and the api-self Deployment."""
        configmap_name = _proxy_configmap_name(sandbox_id)
        secret_name = _secret_env_name()
        env_from: list[dict[str, Any]] = [{"secretRef": {"name": secret_name}}]
        bootstrap_secret_name = _bootstrap_secret_name()
        if (
            os.getenv("KUBERNETES_FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")
            == "onepassword"
            and bootstrap_secret_name
        ):
            env_from.append({"secretRef": {"name": bootstrap_secret_name}})
        proxy_ports: list[dict[str, Any]] = [
            {"containerPort": _proxy_port(), "name": "proxy"},
            {"containerPort": _proxy_management_port(), "name": "management"},
            {"containerPort": _proxy_health_port(), "name": "health"},
        ]
        for name, port in sorted(pg_listen_ports.items(), key=lambda item: item[1]):
            proxy_ports.append(
                {
                    "containerPort": port,
                    "name": f"pg-{name[:11].lower().replace('_', '-')}",
                }
            )
        if core is not None:
            proxy_ports.append({"containerPort": core["port"], "name": "pg-core"})
        return {
            "automountServiceAccountToken": False,
            "restartPolicy": restart_policy,
            "imagePullSecrets": _image_pull_secrets(),
            "containers": [
                {
                    "name": "iron-proxy",
                    "image": _proxy_image(),
                    "imagePullPolicy": _proxy_image_pull_policy(),
                    "env": _proxy_iron_env(secret_name, pg_secrets, core=core),
                    "envFrom": env_from,
                    "ports": proxy_ports,
                    "readinessProbe": {
                        "httpGet": {
                            "path": "/healthz",
                            "port": _proxy_health_port(),
                        },
                        "periodSeconds": 5,
                        "failureThreshold": 30,
                    },
                    "livenessProbe": {
                        "httpGet": {
                            "path": "/healthz",
                            "port": _proxy_health_port(),
                        }
                    },
                    "securityContext": {
                        "allowPrivilegeEscalation": False,
                        "capabilities": {"drop": ["ALL"]},
                        "seccompProfile": {"type": "RuntimeDefault"},
                    },
                    "volumeMounts": [
                        {
                            "name": "iron-proxy-config-rendered",
                            "mountPath": "/etc/iron-proxy-rendered",
                            "readOnly": True,
                        },
                        {
                            "name": "iron-proxy-config",
                            "mountPath": "/etc/iron-proxy",
                        },
                        {"name": "iron-proxy-certs", "mountPath": "/certs"},
                        {
                            "name": "iron-proxy-ca",
                            "mountPath": "/etc/iron-proxy-ca",
                            "readOnly": True,
                        },
                    ],
                    # Copy the read-only rendered config into the writable
                    # /etc/iron-proxy mount where the entrypoint expects it.
                    # iron-proxy's entrypoint script writes the CA cert/key
                    # into the same directory.
                    "command": ["/bin/sh", "-ec"],
                    "args": [
                        "cp /etc/iron-proxy-rendered/proxy.yaml /etc/iron-proxy/proxy.yaml && exec /entrypoint.sh"
                    ],
                },
            ],
            "volumes": [
                {
                    "name": "iron-proxy-config-rendered",
                    "configMap": {"name": configmap_name},
                },
                {"name": "iron-proxy-config", "emptyDir": {}},
                {"name": "iron-proxy-certs", "emptyDir": {}},
                {
                    "name": "iron-proxy-ca",
                    "secret": {"secretName": _firewall_ca_key_secret_name()},
                },
            ],
        }

    async def _create_proxy_pod(
        self,
        sandbox_id: str,
        pg_secrets: list[tuple[PgDsnSecret, str]],
        pg_listen_ports: dict[str, int],
        core: dict[str, Any] | None = None,
    ) -> str:
        proxy_pod_name = _new_proxy_pod_name(sandbox_id)
        spec = self._build_proxy_pod_spec(
            sandbox_id, pg_secrets, pg_listen_ports, restart_policy="Never", core=core
        )
        await self._core_api().create_namespaced_pod(
            _namespace(),
            {
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {
                    "name": proxy_pod_name,
                    "labels": {
                        _PROXY_LABEL: "true",
                        "centaur.ai/sandbox-id": sandbox_id,
                    },
                },
                "spec": spec,
            },
        )
        return proxy_pod_name

    async def _apply_api_proxy_deployment(
        self,
        pg_secrets: list[tuple[PgDsnSecret, str]],
        pg_listen_ports: dict[str, int],
        config_hash: str,
    ) -> None:
        """Create or replace the api-self iron-proxy Deployment.

        Uses ``config_hash`` as a pod-template annotation so a changed
        ConfigMap triggers a rolling restart even though the rest of the
        template is unchanged. ``maxUnavailable: 0`` keeps the proxy
        reachable through the rollout.
        """
        name = _proxy_pod_name(_API_PROXY_SANDBOX_ID)
        labels = {
            _PROXY_LABEL: "true",
            "centaur.ai/sandbox-id": _API_PROXY_SANDBOX_ID,
        }
        pod_spec = self._build_proxy_pod_spec(
            _API_PROXY_SANDBOX_ID,
            pg_secrets,
            pg_listen_ports,
            restart_policy="Always",
        )
        body = {
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {"name": name, "labels": labels},
            "spec": {
                "replicas": 1,
                "strategy": {
                    "type": "RollingUpdate",
                    "rollingUpdate": {"maxSurge": 1, "maxUnavailable": 0},
                },
                "selector": {"matchLabels": labels},
                "template": {
                    "metadata": {
                        "labels": labels,
                        "annotations": {"centaur.ai/config-hash": config_hash},
                    },
                    "spec": pod_spec,
                },
            },
        }
        try:
            await self._apps_api().read_namespaced_deployment(name, _namespace())
        except Exception as exc:
            if not self._is_not_found(exc):
                raise
            await self._apps_api().create_namespaced_deployment(_namespace(), body)
            return
        await self._apps_api().replace_namespaced_deployment(name, _namespace(), body)

    async def _wait_pod_ready(self, pod_name: str) -> float:
        deadline = time.monotonic() + _READY_TIMEOUT_S
        while time.monotonic() < deadline:
            pod = await self._core_api().read_namespaced_pod(pod_name, _namespace())
            phase = (pod.status.phase or "").lower()
            if phase in {"failed", "succeeded"}:
                raise RuntimeError(
                    f"pod exited before ready (pod={pod_name}, phase={phase})"
                )
            if phase == "running":
                conditions = pod.status.conditions or []
                if any(
                    (condition.type or "").lower() == "ready"
                    and (condition.status or "").lower() == "true"
                    for condition in conditions
                ):
                    return round(_READY_TIMEOUT_S - (deadline - time.monotonic()), 3)
            await asyncio.sleep(0.5)
        raise TimeoutError(
            f"pod readiness timed out after {_READY_TIMEOUT_S}s: {pod_name}"
        )

    async def _wait_ready(self, pod_name: str) -> float:
        deadline = time.monotonic() + _READY_TIMEOUT_S
        while time.monotonic() < deadline:
            try:
                pod = await self._core_api().read_namespaced_pod(pod_name, _namespace())
            except Exception as exc:
                if self._is_not_found(exc):
                    await asyncio.sleep(0.5)
                    continue
                raise
            if (
                getattr(getattr(pod, "metadata", None), "deletion_timestamp", None)
                is not None
            ):
                await asyncio.sleep(0.5)
                continue
            phase = (pod.status.phase or "").lower()
            if phase in {"failed", "succeeded"}:
                raise RuntimeError(f"sandbox pod exited before ready (phase={phase})")
            if phase == "running":
                conditions = pod.status.conditions or []
                if any(
                    (condition.type or "").lower() == "ready"
                    and (condition.status or "").lower() == "true"
                    for condition in conditions
                ):
                    return round(_READY_TIMEOUT_S - (deadline - time.monotonic()), 3)
                try:
                    exit_code, _ = await self.exec_run(
                        pod_name,
                        ["test", "-f", "/home/agent/.ready"],
                    )
                    if exit_code == 0:
                        return round(
                            _READY_TIMEOUT_S - (deadline - time.monotonic()), 3
                        )
                except Exception:
                    pass
            await asyncio.sleep(0.5)
        raise TimeoutError(f"sandbox readiness timed out after {_READY_TIMEOUT_S}s")

    async def create(
        self,
        thread_key: str,
        harness: str,
        engine: str,
        *,
        persona: str | None = None,
        repo: str | None = None,
        warm: bool = False,
        model: str | None = None,
        resume_thread_id: str | None = None,
        trace_id: str | None = None,
    ) -> SandboxSession:
        _ensure_kubernetes_env()
        await self._ensure_clients()

        repos_path = _repos_path()
        if repo and not repos_path:
            raise ValueError("REPOS_PATH is required when AGENT_REPO is set")

        runtime_key = f"{thread_key}:{uuid.uuid4().hex[:8]}"
        pod_name = _resource_name("centaur-centaur-sandbox", runtime_key)
        secret_name = _prompt_secret_name(pod_name)
        firewall_host = _proxy_service_name(pod_name)

        secrets = self._secrets_for_sandbox(engine, sandbox_extra_env_map())
        pg_listen_ports = assign_pg_listen_ports(secrets)
        pg_secrets = self._resolved_pg_secrets(secrets)
        sandbox_pg_dsns = {
            secret.name: _build_proxied_pg_url(
                firewall_host,
                pg_listen_ports[secret.name],
                proxy_password,
                secret.database,
            )
            for secret, proxy_password in pg_secrets
        }
        # Core-DB listener for the tool-server sidecar (sidecar-only; never put
        # into sandbox_pg_dsns / the agent env). None when no sidecar runs.
        core_pg = (
            _build_core_pg(firewall_host, pg_listen_ports)
            if _tool_server_image()
            else None
        )

        env = container_env(
            thread_key,
            pod_name,
            firewall_host,
            trace_id=trace_id,
            resume_thread_id=resume_thread_id,
            pg_dsns=sandbox_pg_dsns,
        )
        overlay_image = _overlay_image()
        if overlay_image:
            env.append(f"CENTAUR_OVERLAY_DIR={_SANDBOX_OVERLAY_DIR}")
        if engine == "claude-code" and model:
            env.append(f"CLAUDE_MODEL={model}")
        if engine == "claude-code" and resume_thread_id:
            env.append(f"CLAUDE_CONTINUE_SESSION_ID={resume_thread_id}")
        if persona:
            env.append(f"AGENT_PERSONA={persona}")
        if repo:
            env.append(f"AGENT_REPO={repo}")

        labels = {
            "centaur.ai/sandbox-id": pod_name,
            "centaur.ai/managed": "true",
            "centaur.ai/harness": re.sub(r"[^a-z0-9-]+", "-", harness.lower()),
            "centaur.ai/engine": re.sub(r"[^a-z0-9-]+", "-", engine.lower()),
        }
        if warm:
            labels["centaur.ai/warm"] = "true"

        volume_mounts = [
            {
                "name": "firewall-ca",
                "mountPath": "/firewall-certs",
                "readOnly": True,
            },
            {
                "name": "prompt-bundle",
                "mountPath": "/home/agent/AGENTS_BASE.md",
                "subPath": "AGENTS_BASE.md",
                "readOnly": True,
            },
        ]
        volumes = [
            {
                "name": "firewall-ca",
                "secret": {"secretName": _firewall_ca_secret_name()},
            },
            {
                "name": "prompt-bundle",
                "secret": {"secretName": secret_name},
            },
        ]
        init_containers: list[dict[str, Any]] = []

        if overlay_image:
            volume_mounts.append(
                {
                    "name": "overlay-root",
                    "mountPath": _SANDBOX_OVERLAY_ROOT,
                    "readOnly": True,
                }
            )
            volumes.append(
                {
                    "name": "overlay-root",
                    "emptyDir": {},
                }
            )
            init_containers.append(
                {
                    "name": "overlay-bootstrap",
                    "image": overlay_image,
                    "imagePullPolicy": _overlay_image_pull_policy(),
                    "command": [
                        "/bin/sh",
                        "-ec",
                        (
                            f'src="{_overlay_image_source_path()}"\n'
                            f'target="{_SANDBOX_OVERLAY_DIR}"\n'
                            'mkdir -p "$target"\n'
                            'cp -R "$src"/. "$target"/'
                        ),
                    ],
                    "volumeMounts": [
                        {
                            "name": "overlay-root",
                            "mountPath": _SANDBOX_OVERLAY_ROOT,
                        }
                    ],
                    "securityContext": {
                        "allowPrivilegeEscalation": False,
                        "capabilities": {"drop": ["ALL"]},
                        "runAsGroup": _AGENT_UID,
                        "runAsNonRoot": True,
                        "runAsUser": _AGENT_UID,
                        "seccompProfile": {"type": "RuntimeDefault"},
                    },
                }
            )

        if repos_path:
            volume_mounts.append(
                {
                    "name": "repos",
                    "mountPath": "/home/agent/github",
                    "readOnly": True,
                }
            )
            volumes.append(
                {
                    "name": "repos",
                    "hostPath": {
                        "path": repos_path,
                        "type": "Directory",
                    },
                }
            )

        self._configure_workload_volumes(volume_mounts, volumes)

        cmd = build_harness_cmd(engine, model)

        containers: list[dict[str, Any]] = [
            {
                "name": _CONTAINER_NAME,
                "image": image(),
                "imagePullPolicy": _image_pull_policy(),
                "args": cmd,
                "stdin": True,
                "securityContext": {
                    "allowPrivilegeEscalation": False,
                    "capabilities": {"drop": ["ALL"]},
                    "runAsGroup": _AGENT_UID,
                    "runAsNonRoot": True,
                    "runAsUser": _AGENT_UID,
                    "seccompProfile": {"type": "RuntimeDefault"},
                },
                "tty": False,
                "workingDir": "/home/agent",
                "env": [
                    {
                        "name": item.split("=", 1)[0],
                        "value": item.split("=", 1)[1],
                    }
                    for item in env
                ],
                "resources": _pod_resources(),
                "volumeMounts": volume_mounts,
            }
        ]
        if _tool_server_image():
            assert core_pg is not None  # set under the same guard above
            containers.append(
                _build_tool_server_container(
                    thread_key=thread_key,
                    container_name=pod_name,
                    firewall_host=firewall_host,
                    api_url=os.getenv("AGENT_API_URL", "http://api:8000"),
                    overlay_mount=(
                        _SANDBOX_OVERLAY_ROOT if overlay_image else None
                    ),
                    database_url=core_pg["dsn"],
                )
            )

        pod_spec: dict[str, Any] = {
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": pod_name,
                "labels": labels,
                "annotations": {
                    "centaur.ai/thread-key": thread_key,
                    "centaur.ai/harness": harness,
                    "centaur.ai/engine": engine,
                },
            },
            "spec": {
                "automountServiceAccountToken": False,
                "restartPolicy": "Never",
                "initContainers": init_containers,
                "containers": containers,
                "volumes": volumes,
            },
        }

        runtime_class_name = _runtime_class_name()
        if runtime_class_name:
            pod_spec["spec"]["runtimeClassName"] = runtime_class_name
        image_pull_secrets = _image_pull_secrets()
        if image_pull_secrets:
            pod_spec["spec"]["imagePullSecrets"] = image_pull_secrets
        service_account_name = _service_account_name()
        if service_account_name:
            pod_spec["spec"]["serviceAccountName"] = service_account_name

        await self._delete_existing_workload(pod_name)
        await self._delete_proxy_resources(pod_name)
        try:
            await self._create_prompt_secret(secret_name, persona)
            await self._create_proxy_configmap(
                pod_name, secrets, pg_listen_ports, core=core_pg
            )
            await self._create_proxy_service(pod_name, pg_listen_ports, core=core_pg)
            await self._create_proxy_network_policies(
                pod_name,
                pg_listen_ports,
                core_port=core_pg["port"] if core_pg else None,
            )
            proxy_pod_name = await self._create_proxy_pod(
                pod_name, pg_secrets, pg_listen_ports, core=core_pg
            )
            await self._wait_pod_ready(proxy_pod_name)
            await self._create_workload(pod_spec)
            await self._wait_ready(pod_name)
        except BaseException:
            # Catch BaseException so asyncio.CancelledError (e.g. from
            # wait_for timing out around create()) still triggers cleanup;
            # otherwise the partially created pod, proxy pod, service,
            # network policies, and prompt secret leak.
            with contextlib.suppress(Exception):
                await self._cleanup_workload_after_create_error(pod_name)
            with contextlib.suppress(Exception):
                await self._delete_proxy_resources(pod_name)
            with contextlib.suppress(Exception):
                await self._delete_prompt_secret(secret_name)
            raise

        session = SandboxSession(
            sandbox_id=pod_name,
            thread_key=thread_key,
            harness=harness,
            engine=engine,
            started_at=time.time(),
            backend_name=self.name,
            trace_id=trace_id or "",
        )
        log.info(
            "sandbox_spawned",
            thread_key=thread_key,
            sandbox=pod_name,
            harness=harness,
            engine=engine,
            warm=warm,
            backend=self.name,
            per_sandbox_proxy=True,
        )
        return session

    async def attach(self, session: SandboxSession, *, logs: bool = False) -> None:
        await self._ensure_clients()
        rt = _get_rt(session)
        if rt.prefetched_stdout is None:
            rt.prefetched_stdout = []
        if logs:
            with contextlib.suppress(Exception):
                history = await self._core_api().read_namespaced_pod_log(
                    session.sandbox_id,
                    _namespace(),
                    container=_CONTAINER_NAME,
                    tail_lines=_ATTACH_LOG_TAIL_LINES,
                )
                rt.prefetched_stdout = [
                    line for line in history.splitlines() if line.strip()
                ]
        if (
            rt.stdout_stream is not None
            and rt.stdin_stream is not None
            and rt.attach_context is not None
        ):
            return
        attach_ctx = await self._ws_core_api().connect_get_namespaced_pod_attach(
            session.sandbox_id,
            _namespace(),
            container=_CONTAINER_NAME,
            stdin=True,
            stdout=True,
            stderr=False,
            tty=False,
            _preload_content=False,
        )
        websocket = await attach_ctx.__aenter__()
        rt.attach_context = attach_ctx
        rt.stdout_stream = websocket
        rt.stdin_stream = websocket
        log.info(
            "sandbox_attached",
            thread_key=session.thread_key,
            sandbox=session.sandbox_id,
            harness=session.harness,
            engine=session.engine,
            logs=logs,
            backend=self.name,
        )

    async def write_stdin(self, session: SandboxSession, obj: dict) -> None:
        rt = _get_rt(session)
        if rt.stdin_stream is None:
            raise RuntimeError("not attached (stdin)")
        payload = json.dumps(obj, separators=(",", ":")) + "\n"
        await rt.stdin_stream.send_bytes(
            bytes([STDIN_CHANNEL]) + payload.encode("utf-8")
        )
        log.info(
            "sandbox_stdin_write",
            thread_key=session.thread_key,
            sandbox=session.sandbox_id,
            harness=session.harness,
            engine=session.engine,
            payload_size_bytes=len(payload.encode("utf-8")),
            backend=self.name,
        )

    async def stream_stdout(self, session: SandboxSession) -> AsyncIterator[str]:
        rt = _get_rt(session)
        if rt.stdout_read_lock.locked():
            log.warning(
                "sandbox_stdout_stream_already_active",
                thread_key=session.thread_key,
                sandbox=session.sandbox_id,
                harness=session.harness,
                engine=session.engine,
                backend=self.name,
            )

        async with rt.stdout_read_lock:
            async for line in self._stream_stdout_unlocked(session):
                yield line

    async def _stream_stdout_unlocked(
        self, session: SandboxSession
    ) -> AsyncIterator[str]:
        rt = _get_rt(session)
        if rt.stdout_stream is None:
            raise RuntimeError("not attached (stdout)")

        if rt.prefetched_stdout:
            prefetched = list(rt.prefetched_stdout)
            rt.prefetched_stdout.clear()
            for line in prefetched:
                yield line

        buf = ""
        while True:
            msg = await rt.stdout_stream.receive()
            if msg.type in {WSMsgType.CLOSE, WSMsgType.CLOSING, WSMsgType.CLOSED}:
                break
            if msg.type not in {WSMsgType.BINARY, WSMsgType.TEXT}:
                continue
            channel, payload = _parse_ws_frame(msg.data)
            if channel == ERROR_CHANNEL:
                if payload.strip():
                    log.warning(
                        "kubernetes_attach_error_frame",
                        thread_key=session.thread_key,
                        sandbox=session.sandbox_id,
                        payload=payload[:200],
                    )
                continue
            if channel != STDOUT_CHANNEL:
                continue
            buf += payload
            while "\n" in buf:
                line, buf = buf.split("\n", 1)
                stripped = line.strip()
                if stripped:
                    yield stripped

    async def stop(self, session: SandboxSession) -> None:
        await self.close_streams(session)
        await self.stop_by_id(session.sandbox_id)
        log.info(
            "sandbox_stopped",
            thread_key=session.thread_key,
            sandbox=session.sandbox_id,
            reason="explicit_stop",
            backend=self.name,
        )

    async def status(self, session: SandboxSession) -> str:
        return await self.status_by_id(session.sandbox_id)

    async def status_by_id(self, sandbox_id: str) -> str:
        await self._ensure_clients()
        try:
            pod = await self._core_api().read_namespaced_pod(sandbox_id, _namespace())
        except Exception as exc:
            if self._is_not_found(exc):
                return "gone"
            raise
        if (
            getattr(getattr(pod, "metadata", None), "deletion_timestamp", None)
            is not None
        ):
            return "stopped"
        phase = (pod.status.phase or "").lower()
        if phase == "running":
            return "running"
        if phase == "pending":
            return "created"
        if phase in {"succeeded", "failed"}:
            return "stopped"
        return phase or "unknown"

    async def stop_by_id(self, sandbox_id: str) -> None:
        await self._ensure_clients()
        await self._delete_pod(sandbox_id)
        await self._delete_prompt_secret(_prompt_secret_name(sandbox_id))
        await self._delete_proxy_resources(sandbox_id)

    async def interrupt_by_id(self, sandbox_id: str) -> None:
        with contextlib.suppress(Exception):
            await self.exec_run(sandbox_id, ["kill", "-USR1", "1"])

    async def close_streams(self, session: SandboxSession) -> None:
        rt = _get_rt(session)
        if rt.attach_context is not None:
            with contextlib.suppress(Exception):
                await rt.attach_context.__aexit__(None, None, None)
            rt.attach_context = None
        rt.stdout_stream = None
        rt.stdin_stream = None

    async def exec_run(
        self,
        sandbox_id: str,
        cmd: list[str],
        *,
        environment: dict | None = None,
        user: str = "",
    ) -> tuple[int, bytes]:
        await self._ensure_clients()
        if user and user != "agent":
            raise NotImplementedError(
                "kubernetes backend only supports execs as the default agent user"
            )

        command = list(cmd)
        if environment:
            command = [
                "env",
                *[f"{key}={value}" for key, value in environment.items()],
                *command,
            ]

        websocket_ctx = await self._ws_core_api().connect_get_namespaced_pod_exec(
            sandbox_id,
            _namespace(),
            command=command,
            container=_CONTAINER_NAME,
            stderr=True,
            stdin=False,
            stdout=True,
            tty=False,
            _preload_content=False,
        )
        output_parts: list[str] = []
        error_data = ""
        async with websocket_ctx as websocket:
            while True:
                msg = await websocket.receive()
                if msg.type in {WSMsgType.CLOSE, WSMsgType.CLOSING, WSMsgType.CLOSED}:
                    break
                if msg.type not in {WSMsgType.BINARY, WSMsgType.TEXT}:
                    continue
                channel, payload = _parse_ws_frame(msg.data)
                if channel in {STDOUT_CHANNEL, STDERR_CHANNEL}:
                    output_parts.append(payload)
                elif channel == ERROR_CHANNEL:
                    error_data += payload
        exit_code = self._ws_api().parse_error_data(error_data) if error_data else 0
        return exit_code, "".join(output_parts).encode("utf-8")

    async def refresh_token_by_id(self, sandbox_id: str, new_token: str) -> None:
        exit_code, _ = await self.exec_run(
            sandbox_id,
            ["sh", "-c", 'printf "%s" "$TOKEN" > /home/agent/.api_key'],
            environment={"TOKEN": new_token},
            user="agent",
        )
        if exit_code != 0:
            log.warning(
                "sandbox_token_refresh_failed", sandbox=sandbox_id, exit_code=exit_code
            )

    async def recover_warm(self, pool_harness: str) -> list[SandboxSession]:
        await self._ensure_clients()
        sessions: list[SandboxSession] = []
        try:
            pod_list = await self._core_api().list_namespaced_pod(
                _namespace(),
                label_selector="centaur.ai/warm=true",
            )
        except Exception:
            return sessions

        for pod in getattr(pod_list, "items", []) or []:
            metadata = getattr(pod, "metadata", None)
            status = getattr(pod, "status", None)
            annotations = getattr(metadata, "annotations", None) or {}
            labels = getattr(metadata, "labels", None) or {}
            pod_name = getattr(metadata, "name", "") or ""
            thread_key = annotations.get("centaur.ai/thread-key", "")

            if not pod_name or not thread_key.startswith("warm-"):
                continue

            if getattr(metadata, "deletion_timestamp", None) is not None:
                with contextlib.suppress(Exception):
                    await self.stop_by_id(pod_name)
                continue

            phase = (getattr(status, "phase", "") or "").lower()
            if phase != "running":
                with contextlib.suppress(Exception):
                    await self.stop_by_id(pod_name)
                continue

            sessions.append(
                SandboxSession(
                    sandbox_id=pod_name,
                    thread_key="",
                    harness=annotations.get("centaur.ai/harness", pool_harness),
                    engine=annotations.get(
                        "centaur.ai/engine", labels.get("centaur.ai/engine", "amp")
                    ),
                    started_at=time.time(),
                    backend_name=self.name,
                )
            )
        return sessions

    async def _wait_deployment_ready(self, name: str) -> float:
        deadline = time.monotonic() + _READY_TIMEOUT_S
        while time.monotonic() < deadline:
            dep = await self._apps_api().read_namespaced_deployment(name, _namespace())
            spec_replicas = (dep.spec.replicas or 0) if dep.spec else 0
            status = dep.status
            ready = getattr(status, "ready_replicas", None) or 0
            updated = getattr(status, "updated_replicas", None) or 0
            if (
                spec_replicas > 0
                and ready >= spec_replicas
                and updated >= spec_replicas
            ):
                return round(_READY_TIMEOUT_S - (deadline - time.monotonic()), 3)
            await asyncio.sleep(0.5)
        raise TimeoutError(
            f"deployment readiness timed out after {_READY_TIMEOUT_S}s: {name}"
        )

    # ── Workflow-run pods (one-shot, trusted code) ─────────────────────

    async def _load_api_container_template(self) -> dict[str, Any]:
        """Snapshot env / envFrom / volumes from a running API pod.

        Workflows are trusted code (not arbitrary agent harnesses), so they
        don't need their own iron-proxy. They run in a one-shot pod that
        clones the API container's runtime config — same secrets, same
        ``HTTPS_PROXY`` pointing at the shared API iron-proxy, same CA mount.
        """
        selector = ",".join(
            f"{k}={v}" for k, v in _api_pod_match_labels().items()
        )
        pods = await self._core_api().list_namespaced_pod(
            _namespace(), label_selector=selector
        )
        items = list(getattr(pods, "items", []) or [])
        if not items:
            raise RuntimeError(
                f"no api pod matches selector {selector!r}; "
                "cannot derive workflow-run pod env"
            )
        chosen = next(
            (
                p
                for p in items
                if (getattr(getattr(p, "status", None), "phase", "") or "").lower()
                == "running"
            ),
            items[0],
        )
        sanitize = self._core_api().api_client.sanitize_for_serialization
        spec = sanitize(chosen.spec) or {}
        api_container: dict[str, Any] | None = None
        for c in spec.get("containers") or []:
            if c.get("name") == "api":
                api_container = c
                break
        if api_container is None:
            raise RuntimeError("api container not found in api pod spec")
        return {
            "env": list(api_container.get("env") or []),
            "envFrom": list(api_container.get("envFrom") or []),
            "volumeMounts": list(api_container.get("volumeMounts") or []),
            "volumes": list(spec.get("volumes") or []),
            "imagePullSecrets": list(spec.get("imagePullSecrets") or []),
            "serviceAccountName": spec.get("serviceAccountName"),
        }

    def _build_workflow_run_pod_spec(
        self, run_id: str, *, api_template: dict[str, Any]
    ) -> dict[str, Any]:
        """Pod spec for a single workflow execution.

        The pod runs ``python -m api.workflow_executor --run-id <run_id>``
        against the API image, inheriting the API container's env (so it
        shares the API's view of secrets and proxy config), and exits when
        the handler returns. Lifecycle is one-shot: ``restartPolicy: Never``.
        """
        pod_name = _workflow_run_pod_name(run_id)

        # Override entries that should differ from the API container's env.
        # The executor's __main__ does not start background loops, but
        # disabling them defensively makes the env unambiguous if a handler
        # imports module-level code that reads these.
        overrides: dict[str, dict[str, Any]] = {
            "EXECUTION_WORKER_ENABLED": {"name": "EXECUTION_WORKER_ENABLED", "value": "0"},
            "WORKFLOW_WORKER_ENABLED": {"name": "WORKFLOW_WORKER_ENABLED", "value": "0"},
            "WARM_POOL_ENABLED": {"name": "WARM_POOL_ENABLED", "value": "0"},
            "PLUGIN_WATCHER_ENABLED": {"name": "PLUGIN_WATCHER_ENABLED", "value": "0"},
            # Already running inside the per-run pod; never recurse.
            "WORKFLOW_RUN_SANDBOX_ENABLED": {
                "name": "WORKFLOW_RUN_SANDBOX_ENABLED",
                "value": "0",
            },
            "CENTAUR_WORKFLOW_RUN_ID": {
                "name": "CENTAUR_WORKFLOW_RUN_ID",
                "value": run_id,
            },
        }
        env_list: list[dict[str, Any]] = []
        seen: set[str] = set()
        for entry in api_template["env"]:
            name = entry.get("name") if isinstance(entry, dict) else None
            if not isinstance(name, str):
                continue
            if name in overrides:
                env_list.append(overrides.pop(name))
            else:
                env_list.append(entry)
            seen.add(name)
        for name, entry in overrides.items():
            if name not in seen:
                env_list.append(entry)

        container: dict[str, Any] = {
            "name": "workflow-executor",
            "image": _workflow_run_image(),
            "imagePullPolicy": _workflow_run_image_pull_policy(),
            "command": ["/app/.venv/bin/python"],
            "args": ["-m", "api.workflow_executor", "--run-id", run_id],
            "env": env_list,
            "envFrom": api_template["envFrom"],
            "securityContext": {
                "allowPrivilegeEscalation": False,
                "capabilities": {"drop": ["ALL"]},
                "seccompProfile": {"type": "RuntimeDefault"},
            },
            "resources": _pod_resources(),
            "volumeMounts": api_template["volumeMounts"],
        }

        spec: dict[str, Any] = {
            "restartPolicy": "Never",
            "automountServiceAccountToken": False,
            "imagePullSecrets": (
                api_template["imagePullSecrets"] or _image_pull_secrets()
            ),
            "containers": [container],
            "volumes": api_template["volumes"],
        }
        service_account = api_template.get("serviceAccountName") or _service_account_name()
        if service_account:
            spec["serviceAccountName"] = service_account

        return {
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": pod_name,
                "labels": {
                    "centaur.ai/managed": "true",
                    "centaur.ai/component": "workflow-run",
                    "centaur.ai/workflow-run-id": run_id,
                },
                "annotations": {
                    "centaur.ai/workflow-run-id": run_id,
                },
            },
            "spec": spec,
        }

    async def spawn_workflow_run(self, run_id: str) -> str:
        """Provision a one-shot workflow executor pod.

        Workflows are trusted code, so this skips the per-run iron-proxy
        bring-up: the pod just clones the API container's env (which already
        routes through the shared API iron-proxy) and runs the executor.
        """
        await self._ensure_clients()
        pod_name = _workflow_run_pod_name(run_id)
        api_template = await self._load_api_container_template()
        pod_spec = self._build_workflow_run_pod_spec(
            run_id, api_template=api_template
        )

        # If a previous attempt for this run_id left a pod behind, clear it.
        await self._delete_pod(pod_name)
        try:
            await self._core_api().create_namespaced_pod(_namespace(), pod_spec)
        except Exception as exc:
            # Idempotent: an existing pod for the same run_id is fine.
            if getattr(exc, "status", None) != 409:
                with contextlib.suppress(Exception):
                    await self._delete_pod(pod_name)
                raise
        return pod_name

    async def wait_workflow_run_terminal(self, pod_name: str) -> str:
        """Block until the workflow-run pod terminates; return its phase."""
        await self._ensure_clients()
        backoff = 0.5
        while True:
            try:
                pod = await self._core_api().read_namespaced_pod(
                    pod_name, _namespace()
                )
            except Exception as exc:
                if self._is_not_found(exc):
                    return "gone"
                raise
            phase = (pod.status.phase or "").lower()
            if phase in {"succeeded", "failed"}:
                return phase
            await asyncio.sleep(backoff)
            backoff = min(backoff * 1.5, 5.0)

    async def cleanup_workflow_run_pod(self, pod_name: str) -> None:
        """Delete a workflow-run pod (idempotent). No per-run proxy to clean up."""
        await self._ensure_clients()
        await self._delete_pod(pod_name)

    async def ensure_api_proxy_pod(self) -> None:
        """Create or update the API server's iron-proxy Deployment.

        Uses a Deployment (single replica, RollingUpdate with maxUnavailable=0)
        so k8s reschedules the pod automatically on node failure or eviction.
        The pod template carries a config-hash annotation; when this method
        re-runs with a changed ConfigMap, the annotation changes and the
        Deployment performs a zero-downtime rolling restart.
        """
        await self._ensure_clients()
        secrets = self._collect_secrets()
        pg_listen_ports = assign_pg_listen_ports(secrets)
        pg_secrets = self._resolved_pg_secrets(secrets)
        rendered = render_proxy_yaml(secrets, pg_listen_ports=pg_listen_ports)
        config_hash = hashlib.sha256(rendered.encode("utf-8")).hexdigest()[:16]

        # Mirror the sandbox pg_dsn wiring onto the API process itself: each
        # PgDsnSecret gets an env var pointing at the API proxy's local
        # postgres listener, matching what sandboxes receive via container_env.
        api_proxy_host = _proxy_service_name(_API_PROXY_SANDBOX_ID)
        for secret, proxy_password in pg_secrets:
            os.environ[secret.name] = _build_proxied_pg_url(
                api_proxy_host,
                pg_listen_ports[secret.name],
                proxy_password,
                secret.database,
            )

        await self._apply_proxy_configmap_data(
            _proxy_configmap_name(_API_PROXY_SANDBOX_ID),
            _API_PROXY_SANDBOX_ID,
            rendered,
        )
        await self._create_proxy_service(_API_PROXY_SANDBOX_ID, pg_listen_ports)
        # Reconcile the iron-token-broker BEFORE the proxy rollout so the
        # proxy pods don't briefly point at a broker whose config hasn't
        # caught up with the latest refresh-token secret set.
        if _token_broker_enabled():
            await self._ensure_token_broker(secrets)
        await self._apply_api_proxy_deployment(pg_secrets, pg_listen_ports, config_hash)
        await self._wait_deployment_ready(_proxy_pod_name(_API_PROXY_SANDBOX_ID))
        log.info(
            "api_proxy_deployment_ready",
            deployment=_proxy_pod_name(_API_PROXY_SANDBOX_ID),
            config_hash=config_hash,
            pg_secrets=[s.name for s, _ in pg_secrets],
        )

    async def _ensure_token_broker(self, secrets: list[SecretDef]) -> None:
        """Reconcile the iron-token-broker ConfigMap and trigger a rollout.

        The chart owns the broker Deployment, Service, and NetworkPolicies;
        the API only writes the ConfigMap content (one credentials[] entry
        per registered ``BrokeredTokenSecret``). When the rendered content
        changes, patch the chart-managed Deployment's pod-template with a
        fresh ``centaur.ai/config-hash`` annotation so kubectl rolls it out
        — same effect as ``kubectl rollout restart`` but idempotent on no-op
        reconciles.
        """
        name = _token_broker_name()
        if not name:
            raise ValueError(
                "iron-token-broker reconcile requires "
                "KUBERNETES_TOKEN_BROKER_NAME to be set"
            )
        rendered = render_broker_yaml(secrets)
        config_hash = hashlib.sha256(rendered.encode("utf-8")).hexdigest()[:16]
        changed = await self._apply_token_broker_configmap(rendered)
        if changed:
            await self._patch_token_broker_config_hash(config_hash)
        log.info(
            "token_broker_reconciled",
            deployment=name,
            config_hash=config_hash,
            rollout_triggered=changed,
        )

    async def _apply_token_broker_configmap(self, rendered: str) -> bool:
        """Create or replace the broker ConfigMap; return True if content changed."""
        name = _token_broker_configmap_name()
        labels = {
            _TOKEN_BROKER_LABEL: "true",
            "app.kubernetes.io/component": "token-broker",
        }
        body = {
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {"name": name, "labels": labels},
            "data": {"iron-token-broker.yaml": rendered},
        }
        try:
            existing = await self._core_api().read_namespaced_config_map(
                name, _namespace()
            )
        except Exception as exc:
            if not self._is_not_found(exc):
                raise
            await self._core_api().create_namespaced_config_map(_namespace(), body)
            return True
        existing_data = (existing.data or {}).get("iron-token-broker.yaml")
        if existing_data == rendered:
            return False
        await self._core_api().replace_namespaced_config_map(name, _namespace(), body)
        return True

    async def _patch_token_broker_config_hash(self, config_hash: str) -> None:
        """Bump the pod-template config-hash annotation to trigger a rollout.

        The chart-managed Deployment uses ``Recreate`` strategy, so this
        terminates the running broker, k8s rolls a fresh pod that re-reads
        the updated ConfigMap. Matches the kubectl ``rollout restart``
        annotation contract (``kubectl.kubernetes.io/restartedAt``) plus
        our own hash for traceability.
        """
        name = _token_broker_name()
        patch = {
            "spec": {
                "template": {
                    "metadata": {
                        "annotations": {
                            "centaur.ai/config-hash": config_hash,
                        }
                    }
                }
            }
        }
        try:
            await self._apps_api().patch_namespaced_deployment(
                name, _namespace(), patch
            )
        except Exception as exc:
            if self._is_not_found(exc):
                # The chart hasn't rolled out the broker Deployment yet (or
                # tokenBroker.enabled was just flipped on but helm upgrade
                # hasn't run). Log and move on — the ConfigMap is in place,
                # so when the Deployment appears it will pick up the latest
                # rendered config on first start.
                log.warning(
                    "token_broker_deployment_not_found",
                    deployment=name,
                    hint="run `helm upgrade` to create the broker Deployment",
                )
                return
            raise

    async def _apply_proxy_configmap_data(
        self, name: str, sandbox_id: str, rendered: str
    ) -> None:
        """Create or patch the proxy ConfigMap with rendered YAML.

        Used for the api-self proxy where deletes-and-recreate would clobber
        a Deployment-managed ConfigMap mid-rollout.
        """
        body = {
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": name,
                "labels": {
                    _PROXY_LABEL: "true",
                    "centaur.ai/sandbox-id": sandbox_id,
                },
            },
            "data": {"proxy.yaml": rendered},
        }
        try:
            await self._core_api().read_namespaced_config_map(name, _namespace())
        except Exception as exc:
            if not self._is_not_found(exc):
                raise
            await self._core_api().create_namespaced_config_map(_namespace(), body)
            return
        await self._core_api().replace_namespaced_config_map(name, _namespace(), body)

    async def rename_by_id(self, sandbox_id: str, new_name: str) -> None:
        raise NotImplementedError(
            f"{self.name} backend does not support renaming sandboxes ({sandbox_id} -> {new_name})"
        )


KubernetesSandboxBackend = KubernetesExecutorBackend
