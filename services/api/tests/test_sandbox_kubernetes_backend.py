from __future__ import annotations

import asyncio
import json
import sys
import types
from types import SimpleNamespace

import pytest
from aiohttp import WSMsgType

from api.sandbox.base import SandboxSession
from api.sandbox.config import container_env as sandbox_container_env
from api.sandbox.kubernetes import (
    KubernetesExecutorBackend,
    STDOUT_CHANNEL,
)
from api.sandbox.kubernetes_agent_sandbox import KubernetesAgentSandboxBackend
from api.sandbox.registry import auto_configure


class FakeCoreApi:
    def __init__(self) -> None:
        self.deleted_secrets: list[tuple[str, str]] = []
        self.deleted_pods: list[tuple[str, str, int]] = []
        self.deleted_services: list[tuple[str, str]] = []
        self.deleted_configmaps: list[tuple[str, str]] = []
        self.deleted_pvcs: list[tuple[str, str]] = []
        self.created_secrets: list[tuple[str, dict]] = []
        self.created_pods: list[tuple[str, dict]] = []
        self.created_services: list[tuple[str, dict]] = []
        self.created_configmaps: list[tuple[str, dict]] = []
        self.patched_configmaps: list[tuple[str, str, dict]] = []
        self.pods_to_read: list[SimpleNamespace] = []
        self.pod_list_items: list[SimpleNamespace] = []
        self.list_pod_calls: list[tuple[str, str]] = []

    async def delete_namespaced_secret(self, name: str, namespace: str) -> None:
        self.deleted_secrets.append((namespace, name))

    async def delete_namespaced_pod(
        self,
        name: str,
        namespace: str,
        grace_period_seconds: int = 5,
    ) -> None:
        self.deleted_pods.append((namespace, name, grace_period_seconds))

    async def create_namespaced_secret(self, namespace: str, body: dict) -> None:
        self.created_secrets.append((namespace, body))

    async def create_namespaced_pod(self, namespace: str, body: dict) -> None:
        self.created_pods.append((namespace, body))

    async def delete_namespaced_service(self, name: str, namespace: str) -> None:
        self.deleted_services.append((namespace, name))

    async def delete_namespaced_persistent_volume_claim(
        self, name: str, namespace: str
    ) -> None:
        self.deleted_pvcs.append((namespace, name))

    async def create_namespaced_service(self, namespace: str, body: dict) -> None:
        self.created_services.append((namespace, body))

    async def delete_namespaced_config_map(self, name: str, namespace: str) -> None:
        self.deleted_configmaps.append((namespace, name))

    async def create_namespaced_config_map(self, namespace: str, body: dict) -> None:
        self.created_configmaps.append((namespace, body))

    async def patch_namespaced_config_map(
        self, name: str, namespace: str, body: dict
    ) -> None:
        self.patched_configmaps.append((namespace, name, body))

    async def read_namespaced_pod(self, name: str, namespace: str) -> SimpleNamespace:  # noqa: ARG002
        if self.pods_to_read:
            pod = self.pods_to_read.pop(0)
            if isinstance(pod, Exception):
                raise pod
            return pod
        raise AssertionError("unexpected read_namespaced_pod call")

    async def list_namespaced_pod(
        self,
        namespace: str,
        label_selector: str = "",
    ) -> SimpleNamespace:
        self.list_pod_calls.append((namespace, label_selector))
        if self.pod_list_items:
            return SimpleNamespace(items=list(self.pod_list_items))
        selector = dict(
            item.split("=", 1) for item in label_selector.split(",") if "=" in item
        )
        items = []
        for _, body in self.created_pods:
            metadata = body.get("metadata", {})
            labels = metadata.get("labels", {})
            if all(labels.get(key) == value for key, value in selector.items()):
                items.append(
                    SimpleNamespace(metadata=SimpleNamespace(name=metadata["name"]))
                )
        return SimpleNamespace(items=items)


class FakeWebSocket:
    def __init__(self, messages: list[SimpleNamespace]) -> None:
        self._messages = iter(messages)
        self.sent: list[bytes] = []

    async def receive(self) -> SimpleNamespace:
        return next(self._messages)

    async def send_bytes(self, payload: bytes) -> None:
        self.sent.append(payload)


class FakeWebSocketContext:
    def __init__(self, websocket: FakeWebSocket) -> None:
        self.websocket = websocket

    async def __aenter__(self) -> FakeWebSocket:
        return self.websocket

    async def __aexit__(self, exc_type, exc, tb) -> None:  # noqa: ANN001
        return None


class FakeWsCoreApi:
    def __init__(self, websocket: FakeWebSocket) -> None:
        self.websocket = websocket
        self.exec_calls: list[tuple[str, str, dict]] = []

    async def connect_get_namespaced_pod_exec(
        self, name: str, namespace: str, **kwargs
    ):
        self.exec_calls.append((name, namespace, kwargs))
        return FakeWebSocketContext(self.websocket)


class FakeNetworkingApi:
    def __init__(self) -> None:
        self.deleted_network_policies: list[tuple[str, str]] = []
        self.created_network_policies: list[tuple[str, dict]] = []

    async def delete_namespaced_network_policy(self, name: str, namespace: str) -> None:
        self.deleted_network_policies.append((namespace, name))

    async def create_namespaced_network_policy(
        self, namespace: str, body: dict
    ) -> None:
        self.created_network_policies.append((namespace, body))


class FakeCustomObjectsApi:
    def __init__(self) -> None:
        self.created: list[tuple[str, str, str, str, dict]] = []
        self.deleted: list[tuple[str, str, str, str, str]] = []
        self.patched: list[tuple[str, str, str, str, str, dict]] = []
        self.patch_kwargs: list[dict[str, object]] = []
        self.objects: dict[str, dict] = {}

    async def create_namespaced_custom_object(
        self,
        group: str,
        version: str,
        namespace: str,
        plural: str,
        body: dict,
    ) -> None:
        self.created.append((group, version, namespace, plural, body))
        self.objects[body["metadata"]["name"]] = body

    async def get_namespaced_custom_object(
        self,
        group: str,  # noqa: ARG002
        version: str,  # noqa: ARG002
        namespace: str,  # noqa: ARG002
        plural: str,  # noqa: ARG002
        name: str,
    ) -> dict:
        if name not in self.objects:
            exc = Exception("not found")
            exc.status = 404  # type: ignore[attr-defined]
            raise exc
        return self.objects[name]

    async def delete_namespaced_custom_object(
        self,
        group: str,
        version: str,
        namespace: str,
        plural: str,
        name: str,
    ) -> None:
        self.deleted.append((group, version, namespace, plural, name))

    async def patch_namespaced_custom_object(
        self,
        group: str,
        version: str,
        namespace: str,
        plural: str,
        name: str,
        body: dict,
        **kwargs: object,
    ) -> None:
        self.patched.append((group, version, namespace, plural, name, body))
        self.patch_kwargs.append(kwargs)
        if name in self.objects:
            self.objects[name].setdefault("spec", {}).update(body.get("spec", {}))


class FakeWsApiClient:
    @staticmethod
    def parse_error_data(error_data: str) -> int:
        return 17 if error_data else 0


@pytest.fixture(autouse=True)
def _default_per_sandbox_proxy_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_KEY_SECRET_NAME", "firewall-ca-key")
    monkeypatch.setenv("KUBERNETES_SECRET_ENV_NAME", "centaur-infra-env")
    monkeypatch.delenv("KUBERNETES_BOOTSTRAP_SECRET_NAME", raising=False)


def test_pod_resources_uses_default_limits_when_unset(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.sandbox.kubernetes import _pod_resources

    monkeypatch.delenv("KUBERNETES_SANDBOX_CPU_LIMIT", raising=False)
    monkeypatch.delenv("KUBERNETES_SANDBOX_MEMORY_LIMIT", raising=False)
    monkeypatch.delenv("KUBERNETES_SANDBOX_CPU_REQUEST", raising=False)
    monkeypatch.delenv("KUBERNETES_SANDBOX_MEMORY_REQUEST", raising=False)

    assert _pod_resources() == {"limits": {"cpu": "2", "memory": "4Gi"}}


def test_pod_resources_allows_explicitly_empty_memory_limit(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.sandbox.kubernetes import _pod_resources

    monkeypatch.setenv("KUBERNETES_SANDBOX_CPU_LIMIT", "4000m")
    monkeypatch.setenv("KUBERNETES_SANDBOX_MEMORY_LIMIT", "")
    monkeypatch.setenv("KUBERNETES_SANDBOX_CPU_REQUEST", "200m")
    monkeypatch.setenv("KUBERNETES_SANDBOX_MEMORY_REQUEST", "256Mi")

    assert _pod_resources() == {
        "limits": {"cpu": "4000m"},
        "requests": {"cpu": "200m", "memory": "256Mi"},
    }


def test_container_env_includes_firewall_host_for_secret_bootstrap(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")

    env = sandbox_container_env(
        "thread-key",
        "sandbox-id",
        "firewall.internal",
        trace_id="00000000-0000-0000-0000-000000000123",
    )
    env_map = dict(item.split("=", 1) for item in env)

    assert "FIREWALL_HOST=firewall.internal" in env
    # iron-proxy rewrites the placeholder mid-flight.
    assert env_map["AMP_API_KEY"] == "AMP_API_KEY"
    assert env_map["OPENAI_API_KEY"] == "OPENAI_API_KEY"
    assert env_map["CENTAUR_TRACE_ID"] == "00000000-0000-0000-0000-000000000123"
    assert env_map["NO_PROXY"] == "localhost,127.0.0.1,firewall.internal,api.internal"
    assert env_map["no_proxy"] == env_map["NO_PROXY"]


def test_container_env_passes_allowed_otel_config(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer%20local-key")
    monkeypatch.setenv("OTEL_RESOURCE_ATTRIBUTES", "deployment.environment=staging")
    monkeypatch.setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://otlp-collector:4318")

    env = sandbox_container_env("thread-key", "sandbox-id", "firewall.internal")
    env_map = dict(item.split("=", 1) for item in env)

    assert env_map["OTEL_EXPORTER_OTLP_HEADERS"] == "authorization=Bearer%20local-key"
    assert env_map["OTEL_RESOURCE_ATTRIBUTES"] == "deployment.environment=staging"
    assert env_map["OTEL_EXPORTER_OTLP_ENDPOINT"] == "http://otlp-collector:4318"
    assert "otlp-collector" in env_map["NO_PROXY"].split(",")


def test_container_env_applies_kubernetes_sandbox_extra_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")
    monkeypatch.setenv(
        "KUBERNETES_SANDBOX_EXTRA_ENV",
        json.dumps(
            [
                {
                    "name": "NO_PROXY",
                    "value": "localhost,127.0.0.1,metrics.internal",
                },
                {
                    "name": "no_proxy",
                    "value": "localhost,127.0.0.1,metrics.internal",
                },
                {
                    "name": "OTEL_EXPORTER_OTLP_ENDPOINT",
                    "value": "http://host.orb.internal:8000",
                },
            ]
        ),
    )

    env = sandbox_container_env("thread-key", "sandbox-id", "firewall.internal")
    env_map = dict(item.split("=", 1) for item in env)

    # The operator's extra host is added, but the critical computed hosts (the
    # firewall proxy and the API host) are retained — extraEnv merges, never
    # replaces, so it can't break sandbox egress.
    no_proxy_hosts = env_map["NO_PROXY"].split(",")
    assert "firewall.internal" in no_proxy_hosts
    assert "api.internal" in no_proxy_hosts
    assert "metrics.internal" in no_proxy_hosts
    assert env_map["no_proxy"] == env_map["NO_PROXY"]
    assert env_map["OTEL_EXPORTER_OTLP_ENDPOINT"] == "http://host.orb.internal:8000"
    assert len([item for item in env if item.startswith("NO_PROXY=")]) == 1
    assert len([item for item in env if item.startswith("no_proxy=")]) == 1


def test_container_env_extra_env_cannot_drop_critical_no_proxy_hosts(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # Regression: an operator NO_PROXY override that omits the API host used to
    # clobber the computed value, routing API calls through iron-proxy (405).
    monkeypatch.setenv("AGENT_API_URL", "http://centaur-centaur-api:8000")
    monkeypatch.setenv(
        "KUBERNETES_SANDBOX_EXTRA_ENV",
        json.dumps(
            [
                {"name": "NO_PROXY", "value": "localhost,127.0.0.1,centaur-api"},
                {"name": "no_proxy", "value": "localhost,127.0.0.1,centaur-api"},
            ]
        ),
    )

    env = sandbox_container_env("thread-key", "sandbox-id", "firewall.internal")
    env_map = dict(item.split("=", 1) for item in env)

    no_proxy_hosts = env_map["NO_PROXY"].split(",")
    assert "centaur-centaur-api" in no_proxy_hosts  # real API host survives
    assert "firewall.internal" in no_proxy_hosts
    assert "centaur-api" in no_proxy_hosts  # operator's extra is still honored


def test_container_env_extra_env_cannot_override_pinned_proxy_vars(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(
        "KUBERNETES_SANDBOX_EXTRA_ENV",
        json.dumps(
            [
                {"name": "HTTPS_PROXY", "value": "http://evil:9999"},
                {"name": "http_proxy", "value": "http://evil:9999"},
                {"name": "REQUESTS_CA_BUNDLE", "value": "/tmp/attacker.pem"},
                {"name": "FIREWALL_HOST", "value": "elsewhere"},
            ]
        ),
    )

    env = sandbox_container_env("thread-key", "sandbox-id", "firewall.internal")
    env_map = dict(item.split("=", 1) for item in env)

    assert env_map["HTTPS_PROXY"] == "http://firewall.internal:8080"
    assert env_map["http_proxy"] == "http://firewall.internal:8080"
    assert env_map["REQUESTS_CA_BUNDLE"] == "/firewall-certs/ca-cert.pem"
    assert env_map["FIREWALL_HOST"] == "firewall.internal"


def test_container_env_adds_extra_otel_endpoint_host_to_no_proxy(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(
        "KUBERNETES_SANDBOX_EXTRA_ENV",
        json.dumps(
            [
                {
                    "name": "OTEL_EXPORTER_OTLP_ENDPOINT",
                    "value": "http://host.orb.internal:8000",
                },
            ]
        ),
    )

    env = sandbox_container_env("thread-key", "sandbox-id", "firewall.internal")
    env_map = dict(item.split("=", 1) for item in env)

    assert "host.orb.internal" in env_map["NO_PROXY"].split(",")


def test_prompt_bundle_includes_live_capability_inventory_guidance(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.sandbox.kubernetes import _prompt_bundle

    monkeypatch.delenv("CENTAUR_OVERLAY_DIR", raising=False)

    prompt = _prompt_bundle(None)

    assert "[Authoritative deployment-capability answers]" in prompt
    assert "prefer a live capability listing over workspace files or memory" in prompt
    assert "partial and non-exhaustive" in prompt
    assert "call agent runtime" in prompt


def test_prompt_bundle_includes_named_skill_resolution_guidance(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.sandbox.kubernetes import _prompt_bundle

    monkeypatch.delenv("CENTAUR_OVERLAY_DIR", raising=False)

    prompt = _prompt_bundle(None)

    assert "[Named skill resolution]" in prompt
    assert (
        "resolve that request against local skill definitions before doing broad semantic matching"
        in prompt
    )
    assert (
        'Treat "exists locally" and "is live in this deployment" as separate questions'
        in prompt
    )
    assert "ask one targeted clarification instead of guessing" in prompt


def test_prompt_bundle_starts_with_active_deployment_block(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path,
) -> None:
    from api.sandbox.kubernetes import _prompt_bundle

    overlay_root = tmp_path / "overlay"
    overlay_prompt_dir = overlay_root / "services" / "sandbox"
    overlay_prompt_dir.mkdir(parents=True)
    (overlay_prompt_dir / "SYSTEM_PROMPT.md").write_text("overlay guidance")
    persona_dir = tmp_path / "personas" / "invest"
    persona_dir.mkdir(parents=True)
    (persona_dir / "INVEST.md").write_text("invest persona guidance")

    fake_app = types.ModuleType("api.app")
    fake_app.get_tool_manager = lambda: SimpleNamespace(
        get_persona=lambda name: (
            SimpleNamespace(
                engine="amp",
                prompt_file="INVEST.md",
                tool_dir=persona_dir,
                prompt_content="fallback guidance",
            )
            if name == "invest"
            else None
        )
    )
    monkeypatch.setitem(sys.modules, "api.app", fake_app)
    monkeypatch.setenv("CENTAUR_OVERLAY_DIR", str(overlay_root))
    monkeypatch.setenv("CENTAUR_OVERLAY_IMAGE", "ghcr.io/example/overlay:sha-test")

    prompt = _prompt_bundle("invest")

    assert prompt.startswith("[Active deployment]\n|Persona: invest (engine: amp)")
    assert "|Overlay loaded: yes" in prompt
    assert "|Overlay mount (sandbox): /home/agent/overlay/org" in prompt
    assert "overlay guidance" in prompt
    assert "invest persona guidance" in prompt
    assert "fallback guidance" not in prompt


@pytest.mark.asyncio
async def test_ensure_clients_disables_proxy_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class FakeApiClient:
        def __init__(self, configuration=None, heartbeat=None) -> None:  # noqa: ANN001
            self.configuration = configuration
            self.heartbeat = heartbeat
            self.rest_client = SimpleNamespace(
                pool_manager=SimpleNamespace(_trust_env=True)
            )

    backend = KubernetesExecutorBackend()
    default_config = object()
    created_clients: list[FakeApiClient] = []

    monkeypatch.setattr(
        "api.sandbox.kubernetes.config.load_incluster_config", lambda: None
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes.client.Configuration.get_default_copy",
        lambda: default_config,
    )

    def fake_api_client(*, configuration):
        client = FakeApiClient(configuration=configuration)
        created_clients.append(client)
        return client

    def fake_ws_api_client(*, configuration, heartbeat):
        client = FakeApiClient(configuration=configuration, heartbeat=heartbeat)
        created_clients.append(client)
        return client

    monkeypatch.setattr("api.sandbox.kubernetes.client.ApiClient", fake_api_client)
    monkeypatch.setattr("api.sandbox.kubernetes.WsApiClient", fake_ws_api_client)
    monkeypatch.setattr(
        "api.sandbox.kubernetes.client.CoreV1Api",
        lambda api_client=None: SimpleNamespace(api_client=api_client),
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes.client.NetworkingV1Api",
        lambda api_client=None: SimpleNamespace(api_client=api_client),
    )

    await backend._ensure_clients()

    assert len(created_clients) == 2
    assert all(
        created_client.configuration is default_config
        for created_client in created_clients
    )
    assert all(
        created_client.rest_client.pool_manager._trust_env is False
        for created_client in created_clients
    )
    assert backend._core.api_client is created_clients[0]
    assert backend._networking.api_client is created_clients[0]
    assert backend._ws_api_client is created_clients[1]
    assert backend._ws_core.api_client is created_clients[1]


@pytest.mark.asyncio
async def test_create_requires_repo_cache_volume_for_repo(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()

    monkeypatch.setenv("AGENT_API_URL", "http://api:8000")
    monkeypatch.setenv("FIREWALL_HOST", "firewall")
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_SECRET_NAME", "firewall-ca")

    async def fake_ensure_clients() -> None:
        return None

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)

    with pytest.raises(ValueError, match="REPOS_PATH is required"):
        await backend.create(
            "slack:C123:123.456",
            "amp",
            "amp",
            repo="paradigmxyz/centaur",
        )


@pytest.mark.asyncio
async def test_create_builds_pod_and_prompt_secret(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_networking = FakeNetworkingApi()
    backend._core = fake_core
    backend._networking = fake_networking

    monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")
    monkeypatch.setenv("DATABASE_URL", "postgres://user:pass@db/centaur")
    monkeypatch.setenv("FIREWALL_HOST", "firewall.internal")
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_SECRET_NAME", "firewall-ca")
    monkeypatch.setenv("REPOS_PATH", "/var/lib/centaur/repos")
    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setenv("KUBERNETES_SANDBOX_RUNTIME_CLASS_NAME", "gvisor")
    monkeypatch.setenv("KUBERNETES_SANDBOX_SERVICE_ACCOUNT_NAME", "sandbox-runner")
    monkeypatch.setenv("CENTAUR_OVERLAY_IMAGE", "ghcr.io/tempoxyz/centaur-tempo:latest")
    monkeypatch.setenv("CENTAUR_OVERLAY_IMAGE_PULL_POLICY", "Always")
    monkeypatch.setenv("CENTAUR_OVERLAY_IMAGE_SOURCE_PATH", "/overlay")
    monkeypatch.setattr(
        "api.sandbox.kubernetes._prompt_bundle",
        lambda persona: f"prompt:{persona}",
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes.container_env",
        lambda *_args, **_kwargs: [
            "CENTAUR_API_URL=http://api.internal:8000",
            "CENTAUR_API_KEY=sandbox-token",
            "CENTAUR_TRACE_ID=00000000-0000-0000-0000-000000000123",
            "AMP_API_KEY=AMP_API_KEY",
        ],
    )

    monkeypatch.setattr(
        "api.sandbox.kubernetes.build_harness_cmd", lambda *_args: ["amp-wrapper"]
    )
    monkeypatch.setattr("api.sandbox.kubernetes.image", lambda: "centaur-agent:test")

    async def fake_ensure_clients() -> None:
        return None

    async def fake_wait_ready(_pod_name: str) -> float:
        return 0.01

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)
    monkeypatch.setattr(backend, "_wait_pod_ready", fake_wait_ready)
    monkeypatch.setattr(backend, "_wait_ready", fake_wait_ready)

    session = await backend.create(
        "slack:C123:123.456",
        "amp",
        "amp",
        persona="eng",
        repo="paradigmxyz/centaur",
        resume_thread_id="T-123",
        trace_id="00000000-0000-0000-0000-000000000123",
    )

    assert session.sandbox_id.startswith("centaur-centaur-sandbox-")
    assert fake_core.created_secrets[0][0] == "centaur-sandbox"
    secret_body = fake_core.created_secrets[0][1]
    assert secret_body["stringData"]["AGENTS_BASE.md"] == "prompt:eng"

    pod_body = fake_core.created_pods[1][1]
    container = pod_body["spec"]["containers"][0]
    env = {item["name"]: item["value"] for item in container["env"]}

    assert pod_body["spec"]["runtimeClassName"] == "gvisor"
    assert pod_body["spec"]["serviceAccountName"] == "sandbox-runner"
    assert container["image"] == "centaur-agent:test"
    assert "command" not in container
    assert container["args"] == ["amp-wrapper"]
    assert container["securityContext"] == {
        "allowPrivilegeEscalation": False,
        "capabilities": {"drop": ["ALL"]},
        "runAsGroup": 1001,
        "runAsNonRoot": True,
        "runAsUser": 1001,
        "seccompProfile": {"type": "RuntimeDefault"},
    }
    assert container["stdin"] is True
    assert container["tty"] is False
    assert env["CENTAUR_API_URL"] == "http://api.internal:8000"
    assert env["CENTAUR_API_KEY"] == "sandbox-token"
    assert env["CENTAUR_TRACE_ID"] == "00000000-0000-0000-0000-000000000123"
    assert env["AMP_API_KEY"] == "AMP_API_KEY"
    assert env["CENTAUR_OVERLAY_DIR"] == "/home/agent/overlay/org"
    assert env["AGENT_PERSONA"] == "eng"
    assert env["AGENT_REPO"] == "paradigmxyz/centaur"
    assert (
        pod_body["metadata"]["annotations"]["centaur.ai/thread-key"]
        == "slack:C123:123.456"
    )
    assert {
        "name": "repos",
        "hostPath": {"path": "/var/lib/centaur/repos", "type": "Directory"},
    } in pod_body["spec"]["volumes"]
    assert any(
        volume["name"] == "overlay-root" for volume in pod_body["spec"]["volumes"]
    )
    assert pod_body["spec"]["initContainers"] == [
        {
            "name": "overlay-bootstrap",
            "image": "ghcr.io/tempoxyz/centaur-tempo:latest",
            "imagePullPolicy": "Always",
            "command": [
                "/bin/sh",
                "-ec",
                'src="/overlay"\n'
                'target="/home/agent/overlay/org"\n'
                'mkdir -p "$target"\n'
                'cp -R "$src"/. "$target"/',
            ],
            "volumeMounts": [
                {
                    "name": "overlay-root",
                    "mountPath": "/home/agent/overlay",
                }
            ],
            "securityContext": {
                "allowPrivilegeEscalation": False,
                "capabilities": {"drop": ["ALL"]},
                "runAsGroup": 1001,
                "runAsNonRoot": True,
                "runAsUser": 1001,
                "seccompProfile": {"type": "RuntimeDefault"},
            },
        }
    ]
    assert any(
        mount["name"] == "repos" and mount["mountPath"] == "/home/agent/github"
        for mount in container["volumeMounts"]
    )
    assert any(
        mount["name"] == "overlay-root" and mount["mountPath"] == "/home/agent/overlay"
        for mount in container["volumeMounts"]
    )


@pytest.mark.asyncio
async def test_create_builds_per_sandbox_proxy_resources(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_networking = FakeNetworkingApi()
    backend._core = fake_core
    backend._networking = fake_networking
    monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")
    monkeypatch.delenv("FIREWALL_HOST", raising=False)
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_SECRET_NAME", "firewall-ca")
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_KEY_SECRET_NAME", "firewall-ca-key")
    monkeypatch.setenv("KUBERNETES_SECRET_ENV_NAME", "centaur-infra-env")
    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setenv("KUBERNETES_IRON_PROXY_IMAGE", "centaur-iron-proxy:test")
    monkeypatch.setenv(
        "KUBERNETES_FIREWALL_MANAGER_IMAGE", "centaur-firewall-manager:test"
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes._prompt_bundle", lambda persona: "prompt"
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes.build_harness_cmd", lambda *_args: ["amp-wrapper"]
    )
    monkeypatch.setattr("api.sandbox.kubernetes.image", lambda: "centaur-agent:test")

    async def fake_ensure_clients() -> None:
        return None

    async def fake_wait_ready(_pod_name: str) -> float:
        return 0.01

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)
    monkeypatch.setattr(backend, "_wait_pod_ready", fake_wait_ready)
    monkeypatch.setattr(backend, "_wait_ready", fake_wait_ready)

    session = await backend.create("slack:C123:123.456", "amp", "amp")

    proxy_service = fake_core.created_services[0][1]
    proxy_pod = fake_core.created_pods[0][1]
    sandbox_pod = fake_core.created_pods[1][1]
    proxy_service_name = proxy_service["metadata"]["name"]
    sandbox_env = {
        item["name"]: item["value"]
        for item in sandbox_pod["spec"]["containers"][0]["env"]
    }

    assert session.sandbox_id == sandbox_pod["metadata"]["name"]
    assert (
        sandbox_pod["metadata"]["labels"]["centaur.ai/sandbox-id"] == session.sandbox_id
    )
    assert sandbox_env["FIREWALL_HOST"] == proxy_service_name
    assert sandbox_env["HTTPS_PROXY"] == f"http://{proxy_service_name}:8080"
    assert (
        sandbox_env["NO_PROXY"]
        == f"localhost,127.0.0.1,{proxy_service_name},api.internal"
    )
    assert proxy_pod["metadata"]["labels"] == {
        "centaur.ai/iron-proxy": "true",
        "centaur.ai/sandbox-id": session.sandbox_id,
    }
    # Iron-proxy is now the only container in the proxy pod (firewall-manager
    # removed; the API server drives the ConfigMap directly).
    assert [container["name"] for container in proxy_pod["spec"]["containers"]] == [
        "iron-proxy",
    ]
    assert proxy_pod["spec"]["containers"][0]["image"] == "centaur-iron-proxy:test"
    assert proxy_pod["spec"]["containers"][0]["readinessProbe"]["periodSeconds"] == 5
    assert (
        proxy_pod["spec"]["containers"][0]["readinessProbe"]["failureThreshold"] == 30
    )
    assert proxy_pod["spec"]["containers"][0]["envFrom"] == [
        {"secretRef": {"name": "centaur-infra-env"}}
    ]
    # ConfigMap with the rendered proxy.yaml is created before the pod.
    assert fake_core.created_configmaps, "proxy ConfigMap not created"
    configmap = fake_core.created_configmaps[0][1]
    assert "proxy.yaml" in configmap["data"]
    # Pod mounts the ConfigMap as the rendered config source.
    volume_names = {v["name"] for v in proxy_pod["spec"]["volumes"]}
    assert "iron-proxy-config-rendered" in volume_names
    assert fake_networking.created_network_policies[0][1]["spec"]["podSelector"][
        "matchLabels"
    ] == {
        "centaur.ai/managed": "true",
        "centaur.ai/sandbox-id": session.sandbox_id,
    }
    assert fake_networking.created_network_policies[1][1]["spec"]["podSelector"][
        "matchLabels"
    ] == {
        "centaur.ai/iron-proxy": "true",
        "centaur.ai/sandbox-id": session.sandbox_id,
    }
    assert not any(
        egress.get("to", [{}])[0]
        .get("podSelector", {})
        .get("matchLabels", {})
        .get("app")
        == "onepassword-connect"
        for egress in fake_networking.created_network_policies[1][1]["spec"]["egress"]
    )

    replacement = await backend.create("slack:C123:123.456", "amp", "amp")
    assert replacement.sandbox_id != session.sandbox_id
    assert (
        fake_core.created_pods[2][1]["metadata"]["name"]
        != proxy_pod["metadata"]["name"]
    )


class FakeAppsApi:
    """Minimal AppsV1Api stand-in. Records create/replace/patch and exposes
    pre-seeded reads via ``deployments_to_read`` (FIFO; Exception items are
    raised instead of returned, matching real client behavior)."""

    def __init__(self) -> None:
        self.patched_deployments: list[tuple[str, str, dict]] = []
        self.created_deployments: list[tuple[str, dict]] = []
        self.replaced_deployments: list[tuple[str, str, dict]] = []
        self.deployments_to_read: list = []

    async def read_namespaced_deployment(self, name: str, namespace: str):  # noqa: ANN201, ARG002
        if not self.deployments_to_read:
            raise AssertionError(
                f"unexpected read_namespaced_deployment({name})"
            )
        item = self.deployments_to_read.pop(0)
        if isinstance(item, Exception):
            raise item
        return item

    async def create_namespaced_deployment(self, namespace: str, body: dict) -> None:
        self.created_deployments.append((namespace, body))

    async def replace_namespaced_deployment(
        self, name: str, namespace: str, body: dict
    ) -> None:
        self.replaced_deployments.append((namespace, name, body))

    async def patch_namespaced_deployment(
        self, name: str, namespace: str, body: dict
    ) -> None:
        self.patched_deployments.append((namespace, name, body))


@pytest.mark.asyncio
async def test_ensure_token_broker_writes_configmap_and_patches_deployment(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.sandbox.kubernetes import KubernetesExecutorBackend
    from api.tool_manager import BrokeredTokenSecret, OAuthFieldSource

    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur")
    monkeypatch.setenv(
        "KUBERNETES_TOKEN_BROKER_NAME", "centaur-centaur-token-broker"
    )
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")

    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_apps = FakeAppsApi()
    backend._core = fake_core
    backend._apps = fake_apps

    # First reconcile: ConfigMap doesn't exist yet, Deployment exists.
    fake_core.pods_to_read = []
    not_found = type("NotFound", (Exception,), {})()
    not_found.status = 404  # type: ignore[attr-defined]
    monkeypatch.setattr(
        backend, "_is_not_found", lambda exc: getattr(exc, "status", 0) == 404
    )

    async def fake_read_cm(name: str, namespace: str):  # noqa: ARG001, ANN202
        raise not_found

    monkeypatch.setattr(
        fake_core, "read_namespaced_config_map", fake_read_cm, raising=False
    )
    fake_apps.deployments_to_read = [object()]  # Deployment exists

    secrets = [
        BrokeredTokenSecret(
            name="codex",
            hosts=("auth.openai.com",),
            fields=(
                ("client_id", OAuthFieldSource("CODEX_CLIENT_ID")),
                ("refresh_token", OAuthFieldSource("CODEX_BLOB")),
            ),
            token_endpoint="https://auth.openai.com/oauth/token",
        ),
    ]
    await backend._ensure_token_broker(secrets)

    # ConfigMap was created with the rendered broker YAML.
    assert len(fake_core.created_configmaps) == 1
    cm = fake_core.created_configmaps[0][1]
    assert cm["metadata"]["name"] == "centaur-centaur-token-broker-config"
    assert "credentials:" in cm["data"]["iron-token-broker.yaml"]
    # Deployment got a config-hash annotation patch.
    assert len(fake_apps.patched_deployments) == 1
    _, dep_name, patch = fake_apps.patched_deployments[0]
    assert dep_name == "centaur-centaur-token-broker"
    annotations = patch["spec"]["template"]["metadata"]["annotations"]
    assert "centaur.ai/config-hash" in annotations
    assert annotations["centaur.ai/config-hash"]


@pytest.mark.asyncio
async def test_ensure_token_broker_skips_rollout_when_config_unchanged(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.broker_config import render_broker_yaml
    from api.sandbox.kubernetes import KubernetesExecutorBackend
    from api.tool_manager import BrokeredTokenSecret, OAuthFieldSource

    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur")
    monkeypatch.setenv(
        "KUBERNETES_TOKEN_BROKER_NAME", "centaur-centaur-token-broker"
    )
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")

    secrets = [
        BrokeredTokenSecret(
            name="codex",
            hosts=("auth.openai.com",),
            fields=(
                ("client_id", OAuthFieldSource("CODEX_CLIENT_ID")),
                ("refresh_token", OAuthFieldSource("CODEX_BLOB")),
            ),
            token_endpoint="https://auth.openai.com/oauth/token",
        ),
    ]
    rendered = render_broker_yaml(secrets)

    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_apps = FakeAppsApi()
    backend._core = fake_core
    backend._apps = fake_apps

    # ConfigMap already has the exact rendered content.
    async def fake_read_cm(name: str, namespace: str):  # noqa: ARG001, ANN202
        return SimpleNamespace(data={"iron-token-broker.yaml": rendered})

    monkeypatch.setattr(
        fake_core, "read_namespaced_config_map", fake_read_cm, raising=False
    )

    async def fake_replace_cm(name: str, namespace: str, body: dict) -> None:  # noqa: ARG001
        raise AssertionError("ConfigMap should not be replaced when content unchanged")

    monkeypatch.setattr(
        fake_core, "replace_namespaced_config_map", fake_replace_cm, raising=False
    )

    await backend._ensure_token_broker(secrets)
    # No rollout triggered: Deployment was never read or patched.
    assert fake_apps.patched_deployments == []
    assert fake_apps.deployments_to_read == []


@pytest.mark.asyncio
async def test_ensure_token_broker_tolerates_missing_deployment(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.sandbox.kubernetes import KubernetesExecutorBackend
    from api.tool_manager import BrokeredTokenSecret, OAuthFieldSource

    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur")
    monkeypatch.setenv(
        "KUBERNETES_TOKEN_BROKER_NAME", "centaur-centaur-token-broker"
    )
    monkeypatch.setenv("FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")

    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_apps = FakeAppsApi()
    backend._core = fake_core
    backend._apps = fake_apps
    not_found = type("NotFound", (Exception,), {})()
    not_found.status = 404  # type: ignore[attr-defined]
    monkeypatch.setattr(
        backend, "_is_not_found", lambda exc: getattr(exc, "status", 0) == 404
    )

    async def fake_read_cm(name: str, namespace: str):  # noqa: ARG001, ANN202
        raise not_found

    monkeypatch.setattr(
        fake_core, "read_namespaced_config_map", fake_read_cm, raising=False
    )

    async def fake_patch_deployment(name: str, namespace: str, body: dict) -> None:  # noqa: ARG001
        raise not_found

    monkeypatch.setattr(
        fake_apps, "patch_namespaced_deployment", fake_patch_deployment, raising=False
    )

    secrets = [
        BrokeredTokenSecret(
            name="codex",
            hosts=("h",),
            fields=(
                ("client_id", OAuthFieldSource("CODEX_CLIENT_ID")),
                ("refresh_token", OAuthFieldSource("CODEX_BLOB")),
            ),
            token_endpoint="https://h/token",
        ),
    ]
    # Should not raise — the ConfigMap is written so the broker picks up
    # the latest config whenever helm upgrade lands.
    await backend._ensure_token_broker(secrets)
    assert len(fake_core.created_configmaps) == 1


def test_proxy_iron_env_omits_broker_when_url_unset(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.sandbox.kubernetes import _proxy_iron_env

    monkeypatch.delenv("KUBERNETES_TOKEN_BROKER_URL", raising=False)
    env = _proxy_iron_env("centaur-infra-env", [])
    names = [e["name"] for e in env]
    assert "IRON_BROKER_URL" not in names
    assert "IRON_BROKER_TOKEN" not in names


def test_proxy_iron_env_injects_broker_when_url_set(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from api.sandbox.kubernetes import _proxy_iron_env

    monkeypatch.setenv(
        "KUBERNETES_TOKEN_BROKER_URL", "http://centaur-token-broker:8181"
    )
    env = _proxy_iron_env("centaur-infra-env", [])
    by_name = {e["name"]: e for e in env}
    assert by_name["IRON_BROKER_URL"]["value"] == (
        "http://centaur-token-broker:8181"
    )
    assert by_name["IRON_BROKER_TOKEN"]["valueFrom"]["secretKeyRef"] == {
        "name": "centaur-infra-env",
        "key": "IRON_BROKER_TOKEN",
    }


@pytest.mark.asyncio
async def test_per_sandbox_proxy_uses_bootstrap_secret_for_onepassword(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    backend._core = fake_core
    monkeypatch.setenv("KUBERNETES_BOOTSTRAP_SECRET_NAME", "centaur-bootstrap")
    monkeypatch.setenv("KUBERNETES_FIREWALL_MANAGER_SECRET_SOURCE", "onepassword")

    async def fake_ensure_clients() -> None:
        return None

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)
    await backend._create_proxy_pod("sandbox-pod", [], {})

    proxy_pod = fake_core.created_pods[0][1]
    assert proxy_pod["spec"]["containers"][0]["envFrom"] == [
        {"secretRef": {"name": "centaur-infra-env"}},
        {"secretRef": {"name": "centaur-bootstrap"}},
    ]


@pytest.mark.asyncio
async def test_per_sandbox_proxy_allows_onepassword_connect_egress(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_networking = FakeNetworkingApi()
    backend._networking = fake_networking
    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setenv(
        "KUBERNETES_FIREWALL_MANAGER_SECRET_SOURCE", "onepassword-connect"
    )
    monkeypatch.setenv("KUBERNETES_OP_CONNECT_APP_NAME", "custom-connect")
    monkeypatch.setenv("KUBERNETES_OP_CONNECT_PORT", "8181")

    await backend._create_proxy_network_policies("sandbox-pod", {})

    proxy_policy = fake_networking.created_network_policies[1][1]
    assert {
        "to": [
            {
                "podSelector": {
                    "matchLabels": {
                        "app": "custom-connect",
                    }
                }
            }
        ],
        "ports": [{"protocol": "TCP", "port": 8181}],
    } in proxy_policy["spec"]["egress"]


@pytest.mark.asyncio
async def test_create_cleans_up_per_sandbox_proxy_when_proxy_readiness_times_out(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_networking = FakeNetworkingApi()
    backend._core = fake_core
    backend._networking = fake_networking
    monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_SECRET_NAME", "firewall-ca")
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_KEY_SECRET_NAME", "firewall-ca-key")
    monkeypatch.setenv("KUBERNETES_SECRET_ENV_NAME", "centaur-infra-env")
    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setattr(
        "api.sandbox.kubernetes._prompt_bundle", lambda persona: "prompt"
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes.build_harness_cmd", lambda *_args: ["amp-wrapper"]
    )
    monkeypatch.setattr("api.sandbox.kubernetes.image", lambda: "centaur-agent:test")

    async def fake_ensure_clients() -> None:
        return None

    async def fail_wait_ready(_pod_name: str) -> float:
        raise TimeoutError("proxy readiness timed out")

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)
    monkeypatch.setattr(backend, "_wait_pod_ready", fail_wait_ready)

    with pytest.raises(TimeoutError, match="proxy readiness timed out"):
        await backend.create("slack:C123:123.456", "amp", "amp")

    sandbox_id = fake_core.created_services[0][1]["metadata"]["labels"][
        "centaur.ai/sandbox-id"
    ]
    assert ("centaur-sandbox", sandbox_id, 5) in fake_core.deleted_pods
    assert (
        "centaur-sandbox",
        fake_core.created_pods[0][1]["metadata"]["name"],
        5,
    ) in fake_core.deleted_pods
    assert (
        "centaur-sandbox",
        fake_core.created_services[0][1]["metadata"]["name"],
    ) in fake_core.deleted_services
    assert fake_networking.deleted_network_policies


@pytest.mark.asyncio
async def test_stop_by_id_removes_per_sandbox_proxy_resources(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_networking = FakeNetworkingApi()
    backend._core = fake_core
    backend._networking = fake_networking
    fake_core.pod_list_items = [
        SimpleNamespace(metadata=SimpleNamespace(name="proxy-pod-unique"))
    ]
    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")

    async def fake_ensure_clients() -> None:
        return None

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)

    await backend.stop_by_id("sandbox-pod")

    assert ("centaur-sandbox", "sandbox-pod", 5) in fake_core.deleted_pods
    assert ("centaur-sandbox", "proxy-pod-unique", 5) in fake_core.deleted_pods
    assert fake_core.list_pod_calls == [
        (
            "centaur-sandbox",
            "centaur.ai/iron-proxy=true,centaur.ai/sandbox-id=sandbox-pod",
        )
    ]
    assert any(
        name.startswith("centaur-centaur-proxy-")
        for _, name, _ in fake_core.deleted_pods
    )
    assert any(
        name.startswith("centaur-centaur-proxy-")
        for _, name in fake_core.deleted_services
    )
    assert len(fake_networking.deleted_network_policies) == 2


@pytest.mark.asyncio
async def test_create_cleans_up_pod_and_prompt_secret_when_readiness_times_out(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_networking = FakeNetworkingApi()
    backend._core = fake_core
    backend._networking = fake_networking

    monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")
    monkeypatch.setenv("FIREWALL_HOST", "firewall.internal")
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_SECRET_NAME", "firewall-ca")
    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setattr(
        "api.sandbox.kubernetes._prompt_bundle", lambda persona: "prompt"
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes.container_env",
        lambda *_args, **_kwargs: ["CENTAUR_API_URL=http://api.internal:8000"],
    )

    monkeypatch.setattr(
        "api.sandbox.kubernetes.build_harness_cmd", lambda *_args: ["amp-wrapper"]
    )
    monkeypatch.setattr("api.sandbox.kubernetes.image", lambda: "centaur-agent:test")

    async def fake_ensure_clients() -> None:
        return None

    async def fake_wait_ready(_pod_name: str) -> float:
        raise TimeoutError("sandbox readiness timed out after 60s")

    async def fake_proxy_wait_ready(_pod_name: str) -> float:
        return 0.01

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)
    monkeypatch.setattr(backend, "_wait_pod_ready", fake_proxy_wait_ready)
    monkeypatch.setattr(backend, "_wait_ready", fake_wait_ready)

    with pytest.raises(TimeoutError, match="readiness timed out"):
        await backend.create("slack:C123:123.456", "amp", "amp")

    pod_name = fake_core.created_pods[1][1]["metadata"]["name"]
    secret_name = fake_core.created_secrets[0][1]["metadata"]["name"]

    assert ("centaur-sandbox", pod_name, 5) in fake_core.deleted_pods
    assert fake_core.deleted_secrets[-1] == ("centaur-sandbox", secret_name)


@pytest.mark.asyncio
async def test_create_cleans_up_when_cancelled_during_readiness(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_networking = FakeNetworkingApi()
    backend._core = fake_core
    backend._networking = fake_networking

    monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")
    monkeypatch.setenv("FIREWALL_HOST", "firewall.internal")
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_SECRET_NAME", "firewall-ca")
    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setattr(
        "api.sandbox.kubernetes._prompt_bundle", lambda persona: "prompt"
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes.container_env",
        lambda *_args, **_kwargs: ["CENTAUR_API_URL=http://api.internal:8000"],
    )

    monkeypatch.setattr(
        "api.sandbox.kubernetes.build_harness_cmd", lambda *_args: ["amp-wrapper"]
    )
    monkeypatch.setattr("api.sandbox.kubernetes.image", lambda: "centaur-agent:test")

    async def fake_ensure_clients() -> None:
        return None

    async def fake_proxy_wait_ready(_pod_name: str) -> float:
        return 0.01

    async def cancel_wait_ready(_pod_name: str) -> float:
        raise asyncio.CancelledError()

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)
    monkeypatch.setattr(backend, "_wait_pod_ready", fake_proxy_wait_ready)
    monkeypatch.setattr(backend, "_wait_ready", cancel_wait_ready)

    with pytest.raises(asyncio.CancelledError):
        await backend.create("slack:C123:123.456", "amp", "amp")

    pod_name = fake_core.created_pods[1][1]["metadata"]["name"]
    secret_name = fake_core.created_secrets[0][1]["metadata"]["name"]

    assert ("centaur-sandbox", pod_name, 5) in fake_core.deleted_pods
    assert fake_core.deleted_secrets[-1] == ("centaur-sandbox", secret_name)
    assert fake_networking.deleted_network_policies


@pytest.mark.asyncio
async def test_create_mounts_repo_cache_host_path(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_networking = FakeNetworkingApi()
    backend._core = fake_core
    backend._networking = fake_networking

    monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")
    monkeypatch.setenv("FIREWALL_HOST", "firewall.internal")
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_SECRET_NAME", "firewall-ca")
    monkeypatch.setenv("REPOS_PATH", "/var/lib/centaur/repos")
    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setattr(
        "api.sandbox.kubernetes._prompt_bundle",
        lambda persona: f"prompt:{persona}",
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes.container_env",
        lambda *_args, **_kwargs: [
            "CENTAUR_API_URL=http://api.internal:8000",
            "CENTAUR_API_KEY=sandbox-token",
        ],
    )

    monkeypatch.setattr(
        "api.sandbox.kubernetes.build_harness_cmd", lambda *_args: ["amp-wrapper"]
    )
    monkeypatch.setattr("api.sandbox.kubernetes.image", lambda: "centaur-agent:test")

    async def fake_ensure_clients() -> None:
        return None

    async def fake_wait_ready(_pod_name: str) -> float:
        return 0.01

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)
    monkeypatch.setattr(backend, "_wait_pod_ready", fake_wait_ready)
    monkeypatch.setattr(backend, "_wait_ready", fake_wait_ready)

    await backend.create(
        "slack:C123:123.456",
        "amp",
        "amp",
        repo="paradigmxyz/centaur",
    )

    pod_body = fake_core.created_pods[1][1]
    container = pod_body["spec"]["containers"][0]

    assert any(
        mount["name"] == "repos"
        and mount["mountPath"] == "/home/agent/github"
        and mount["readOnly"] is True
        for mount in container["volumeMounts"]
    )
    assert {
        "name": "repos",
        "hostPath": {"path": "/var/lib/centaur/repos", "type": "Directory"},
    } in pod_body["spec"]["volumes"]


@pytest.mark.asyncio
async def test_create_can_use_agent_sandbox_with_state_volume(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesAgentSandboxBackend()
    fake_core = FakeCoreApi()
    fake_networking = FakeNetworkingApi()
    fake_custom = FakeCustomObjectsApi()
    backend._core = fake_core
    backend._networking = fake_networking
    backend._custom = fake_custom

    monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")
    monkeypatch.setenv("FIREWALL_HOST", "firewall.internal")
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_SECRET_NAME", "firewall-ca")
    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setenv("KUBERNETES_SANDBOX_CONTROLLER", "agent-sandbox")
    monkeypatch.setenv("KUBERNETES_SANDBOX_STATE_VOLUME_ENABLED", "1")
    monkeypatch.setenv("KUBERNETES_SANDBOX_STATE_VOLUME_SIZE", "7Gi")
    monkeypatch.setenv("KUBERNETES_SANDBOX_STATE_VOLUME_STORAGE_CLASS", "local-path")
    monkeypatch.setattr(
        "api.sandbox.kubernetes._prompt_bundle", lambda persona: "prompt"
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes.container_env",
        lambda *_args, **_kwargs: ["CENTAUR_API_URL=http://api.internal:8000"],
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes.build_harness_cmd", lambda *_args: ["amp-wrapper"]
    )
    monkeypatch.setattr("api.sandbox.kubernetes.image", lambda: "centaur-agent:test")

    async def fake_ensure_clients() -> None:
        return None

    async def fake_wait_ready(_pod_name: str) -> float:
        return 0.01

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)
    monkeypatch.setattr(backend, "_wait_pod_ready", fake_wait_ready)
    monkeypatch.setattr(backend, "_wait_ready", fake_wait_ready)

    session = await backend.create("slack:C123:123.456", "amp", "amp")

    assert len(fake_core.created_pods) == 1
    assert (
        fake_core.created_pods[0][1]["metadata"]["labels"]["centaur.ai/iron-proxy"]
        == "true"
    )
    assert len(fake_custom.created) == 1
    group, version, namespace, plural, sandbox_body = fake_custom.created[0]
    assert (group, version, namespace, plural) == (
        "agents.x-k8s.io",
        "v1alpha1",
        "centaur-sandbox",
        "sandboxes",
    )
    assert sandbox_body["metadata"]["name"] == session.sandbox_id
    assert sandbox_body["spec"]["replicas"] == 1
    assert sandbox_body["spec"]["shutdownPolicy"] == "Retain"
    claim_template = sandbox_body["spec"]["volumeClaimTemplates"][0]
    assert claim_template["metadata"]["name"] == "state"
    assert claim_template["spec"]["resources"]["requests"]["storage"] == "7Gi"
    assert claim_template["spec"]["storageClassName"] == "local-path"

    pod_template = sandbox_body["spec"]["podTemplate"]
    sandbox_container = pod_template["spec"]["containers"][0]
    assert any(
        mount["name"] == "state" and mount["mountPath"] == "/home/agent/state"
        for mount in sandbox_container["volumeMounts"]
    )
    assert all(
        volume["name"] != "state" for volume in pod_template["spec"].get("volumes", [])
    )


@pytest.mark.asyncio
async def test_agent_sandbox_pause_resume_and_stop(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesAgentSandboxBackend()
    fake_core = FakeCoreApi()
    fake_networking = FakeNetworkingApi()
    fake_custom = FakeCustomObjectsApi()
    backend._core = fake_core
    backend._networking = fake_networking
    backend._custom = fake_custom

    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setenv("KUBERNETES_SANDBOX_CONTROLLER", "agent-sandbox")

    async def fake_ensure_clients() -> None:
        return None

    async def fake_wait_ready(_pod_name: str) -> float:
        return 0.01

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)
    monkeypatch.setattr(backend, "_wait_ready", fake_wait_ready)

    await backend.pause_by_id("sandbox-1")
    await backend.resume_by_id("sandbox-1")
    await backend.stop_by_id("sandbox-1")

    assert fake_custom.patched == [
        (
            "agents.x-k8s.io",
            "v1alpha1",
            "centaur-sandbox",
            "sandboxes",
            "sandbox-1",
            {"spec": {"replicas": 0}},
        ),
        (
            "agents.x-k8s.io",
            "v1alpha1",
            "centaur-sandbox",
            "sandboxes",
            "sandbox-1",
            {"spec": {"replicas": 1}},
        ),
    ]
    assert fake_custom.patch_kwargs == [
        {"_content_type": "application/merge-patch+json"},
        {"_content_type": "application/merge-patch+json"},
    ]
    assert fake_custom.deleted == [
        (
            "agents.x-k8s.io",
            "v1alpha1",
            "centaur-sandbox",
            "sandboxes",
            "sandbox-1",
        )
    ]
    assert fake_core.deleted_pvcs == [("centaur-sandbox", "state-sandbox-1")]


@pytest.mark.asyncio
async def test_agent_sandbox_status_uses_sandbox_replicas_for_suspended_and_resuming(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesAgentSandboxBackend()
    fake_core = FakeCoreApi()
    fake_custom = FakeCustomObjectsApi()
    backend._core = fake_core
    backend._custom = fake_custom

    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setenv("KUBERNETES_SANDBOX_CONTROLLER", "agent-sandbox")

    async def fake_ensure_clients() -> None:
        return None

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)

    fake_custom.objects["sandbox-1"] = {
        "metadata": {"name": "sandbox-1"},
        "spec": {"replicas": 0},
    }
    not_found = Exception("not found")
    not_found.status = 404  # type: ignore[attr-defined]
    fake_core.pods_to_read = [not_found]
    assert await backend.status_by_id("sandbox-1") == "suspended"

    fake_custom.objects["sandbox-1"]["spec"]["replicas"] = 1
    deleting_pod = SimpleNamespace(
        metadata=SimpleNamespace(deletion_timestamp="2026-05-23T14:00:00Z"),
        status=SimpleNamespace(phase="Running", conditions=[]),
    )
    fake_core.pods_to_read = [deleting_pod]
    assert await backend.status_by_id("sandbox-1") == "created"


@pytest.mark.asyncio
async def test_wait_ready_ignores_terminating_pod(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    backend._core = fake_core

    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")

    async def fake_ensure_clients() -> None:
        return None

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)

    terminating_pod = SimpleNamespace(
        metadata=SimpleNamespace(deletion_timestamp="2026-05-23T14:00:00Z"),
        status=SimpleNamespace(
            phase="Running",
            conditions=[SimpleNamespace(type="Ready", status="True")],
        ),
    )
    ready_pod = SimpleNamespace(
        metadata=SimpleNamespace(deletion_timestamp=None),
        status=SimpleNamespace(
            phase="Running",
            conditions=[SimpleNamespace(type="Ready", status="True")],
        ),
    )
    fake_core.pods_to_read = [terminating_pod, ready_pod]

    assert await backend._wait_ready("sandbox-1") >= 0


@pytest.mark.asyncio
async def test_exec_run_prefixes_environment_and_collects_output(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    websocket = FakeWebSocket(
        [
            SimpleNamespace(
                type=WSMsgType.BINARY, data=bytes([STDOUT_CHANNEL]) + b"hello\n"
            ),
            SimpleNamespace(type=WSMsgType.CLOSED, data=b""),
        ]
    )
    backend = KubernetesExecutorBackend()
    backend._ws_core = FakeWsCoreApi(websocket)
    backend._ws_api_client = FakeWsApiClient()

    async def fake_ensure_clients() -> None:
        return None

    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)

    exit_code, output = await backend.exec_run(
        "sandbox-pod",
        ["sh", "-c", "echo hello"],
        environment={"TOKEN": "sandbox-token"},
        user="agent",
    )

    call = backend._ws_core.exec_calls[0]
    assert call[0] == "sandbox-pod"
    assert call[1] == "centaur-sandbox"
    assert call[2]["command"][:3] == ["env", "TOKEN=sandbox-token", "sh"]
    assert exit_code == 0
    assert output == b"hello\n"


@pytest.mark.asyncio
async def test_wait_ready_uses_pod_ready_condition_before_exec_probe(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_core.pods_to_read.append(
        SimpleNamespace(
            status=SimpleNamespace(
                phase="Running",
                conditions=[SimpleNamespace(type="Ready", status="True")],
            )
        )
    )
    backend._core = fake_core

    async def unexpected_exec_run(*_args, **_kwargs):  # noqa: ANN002, ANN003
        raise AssertionError("exec_run should not be called when pod is already Ready")

    monkeypatch.setattr(backend, "exec_run", unexpected_exec_run)

    waited = await backend._wait_ready("sandbox-pod")

    assert waited >= 0


@pytest.mark.asyncio
async def test_status_by_id_returns_stopped_for_terminating_pod(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    fake_core.pods_to_read.append(
        SimpleNamespace(
            metadata=SimpleNamespace(deletion_timestamp="2026-04-21T15:00:00Z"),
            status=SimpleNamespace(phase="Running"),
        )
    )
    backend._core = fake_core

    async def fake_ensure_clients() -> None:
        return None

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)

    status = await backend.status_by_id("sandbox-pod")

    assert status == "stopped"


@pytest.mark.asyncio
async def test_stream_stdout_yields_prefetched_and_live_lines() -> None:
    from api.agent import _drop_runtime, _get_runtime

    session = SandboxSession(
        sandbox_id="sandbox-pod",
        thread_key="slack:C123:123.456",
        harness="amp",
        engine="amp",
    )
    _drop_runtime(session.sandbox_id)
    rt = _get_runtime(session.sandbox_id)
    rt.prefetched_stdout = ["prefetched line"]
    rt.stdout_stream = FakeWebSocket(
        [
            SimpleNamespace(
                type=WSMsgType.BINARY, data=bytes([STDOUT_CHANNEL]) + b"live line\n"
            ),
            SimpleNamespace(type=WSMsgType.CLOSED, data=b""),
        ]
    )

    backend = KubernetesExecutorBackend()
    lines = [line async for line in backend.stream_stdout(session)]

    assert lines == ["prefetched line", "live line"]
    _drop_runtime(session.sandbox_id)


@pytest.mark.asyncio
async def test_stream_stdout_serializes_concurrent_readers() -> None:
    from api.agent import _drop_runtime, _get_runtime

    class BlockingWebSocket:
        def __init__(self) -> None:
            self.in_receive = False
            self.receive_started = asyncio.Event()
            self.release_receive = asyncio.Event()
            self.receive_calls = 0

        async def receive(self) -> SimpleNamespace:
            if self.in_receive:
                raise AssertionError("concurrent receive")
            self.in_receive = True
            self.receive_calls += 1
            self.receive_started.set()
            await self.release_receive.wait()
            self.in_receive = False
            return SimpleNamespace(type=WSMsgType.CLOSED, data=b"")

    session = SandboxSession(
        sandbox_id="sandbox-pod",
        thread_key="slack:C123:123.456",
        harness="amp",
        engine="amp",
    )
    _drop_runtime(session.sandbox_id)
    rt = _get_runtime(session.sandbox_id)
    websocket = BlockingWebSocket()
    rt.stdout_stream = websocket

    backend = KubernetesExecutorBackend()
    first = asyncio.create_task(asyncio.wait_for(_collect_stdout(backend, session), 1))
    await websocket.receive_started.wait()
    second = asyncio.create_task(asyncio.wait_for(_collect_stdout(backend, session), 1))
    await asyncio.sleep(0)

    assert websocket.receive_calls == 1
    assert not second.done()

    websocket.release_receive.set()
    assert await first == []
    assert await second == []
    assert websocket.receive_calls == 2
    _drop_runtime(session.sandbox_id)


async def _collect_stdout(
    backend: KubernetesExecutorBackend,
    session: SandboxSession,
) -> list[str]:
    return [line async for line in backend.stream_stdout(session)]


def test_auto_configure_selects_kubernetes_backend(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("KUBERNETES_SANDBOX_CONTROLLER", raising=False)

    backend = auto_configure()

    assert isinstance(backend, KubernetesExecutorBackend)
    assert not isinstance(backend, KubernetesAgentSandboxBackend)


def test_auto_configure_selects_agent_sandbox_backend(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("KUBERNETES_SANDBOX_CONTROLLER", "agent-sandbox")

    backend = auto_configure()

    assert isinstance(backend, KubernetesAgentSandboxBackend)


def test_kubernetes_backend_supports_warm_pool() -> None:
    backend = KubernetesExecutorBackend()

    assert backend.supports_warm_pool is True


@pytest.mark.asyncio
async def test_recover_warm_returns_running_warm_pods_and_cleans_stale(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    backend._core = fake_core

    fake_core.pod_list_items = [
        SimpleNamespace(
            metadata=SimpleNamespace(
                name="centaur-sandbox-warm-running",
                deletion_timestamp=None,
                annotations={
                    "centaur.ai/thread-key": "warm-123",
                    "centaur.ai/harness": "amp",
                    "centaur.ai/engine": "amp",
                },
                labels={"centaur.ai/warm": "true"},
            ),
            status=SimpleNamespace(phase="Running"),
        ),
        SimpleNamespace(
            metadata=SimpleNamespace(
                name="centaur-sandbox-warm-finished",
                deletion_timestamp=None,
                annotations={
                    "centaur.ai/thread-key": "warm-456",
                    "centaur.ai/harness": "amp",
                    "centaur.ai/engine": "amp",
                },
                labels={"centaur.ai/warm": "true"},
            ),
            status=SimpleNamespace(phase="Succeeded"),
        ),
        SimpleNamespace(
            metadata=SimpleNamespace(
                name="centaur-sandbox-non-placeholder",
                deletion_timestamp=None,
                annotations={
                    "centaur.ai/thread-key": "slack:C123:123.456",
                    "centaur.ai/harness": "amp",
                    "centaur.ai/engine": "amp",
                },
                labels={"centaur.ai/warm": "true"},
            ),
            status=SimpleNamespace(phase="Running"),
        ),
    ]

    async def fake_ensure_clients() -> None:
        return None

    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)

    sessions = await backend.recover_warm("amp")

    assert [session.sandbox_id for session in sessions] == [
        "centaur-sandbox-warm-running"
    ]
    assert sessions[0].backend_name == "kubernetes"
    assert (
        "centaur-sandbox",
        "centaur-sandbox-warm-finished",
        5,
    ) in fake_core.deleted_pods
    assert (
        "centaur-sandbox",
        "centaur-sandbox-warm-finished-cfg",
    ) in fake_core.deleted_secrets


def _stub_create_dependencies(
    monkeypatch: pytest.MonkeyPatch,
    backend: "KubernetesExecutorBackend",
    *,
    extra_env: list[dict[str, str]] | None = None,
    harness_cmd: str = "claude-app-wrapper",
) -> None:
    """Common environment setup for create()-flow integration tests."""
    from api.tool_manager import ToolManager

    monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")
    monkeypatch.delenv("FIREWALL_HOST", raising=False)
    monkeypatch.setenv("KUBERNETES_FIREWALL_CA_SECRET_NAME", "firewall-ca")
    monkeypatch.setenv("KUBERNETES_NAMESPACE", "centaur-sandbox")
    monkeypatch.setenv("KUBERNETES_IRON_PROXY_IMAGE", "centaur-iron-proxy:test")
    if extra_env is None:
        monkeypatch.delenv("KUBERNETES_SANDBOX_EXTRA_ENV", raising=False)
    else:
        monkeypatch.setenv("KUBERNETES_SANDBOX_EXTRA_ENV", json.dumps(extra_env))
    monkeypatch.setattr(
        "api.sandbox.kubernetes._prompt_bundle", lambda persona: "prompt"
    )
    monkeypatch.setattr(
        "api.sandbox.kubernetes.build_harness_cmd", lambda *_args: [harness_cmd]
    )
    monkeypatch.setattr("api.sandbox.kubernetes.image", lambda: "centaur-agent:test")

    # Bypass tool discovery (which needs DATABASE_URL) by instantiating a
    # ToolManager with an empty tool table. The harness-secret selection
    # logic doesn't depend on loaded tools.
    bare_tm = ToolManager.__new__(ToolManager)
    bare_tm.tools = {}
    fake_app = types.ModuleType("api.app")
    fake_app.get_tool_manager = lambda: bare_tm
    monkeypatch.setitem(sys.modules, "api.app", fake_app)

    async def fake_ensure_clients() -> None:
        return None

    async def fake_wait_ready(_pod_name: str) -> float:
        return 0.01

    monkeypatch.setattr(backend, "_ensure_clients", fake_ensure_clients)
    monkeypatch.setattr(backend, "_wait_pod_ready", fake_wait_ready)
    monkeypatch.setattr(backend, "_wait_ready", fake_wait_ready)


@pytest.mark.asyncio
async def test_create_uses_brokered_creds_for_claude_oauth(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """claude-code + CLAUDE_CODE_AUTH_MODE=access_token must publish the
    brokered ``anthropic-claude`` credential into the per-sandbox iron-proxy
    configmap and must NOT include ``ANTHROPIC_API_KEY``."""
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    backend._core = fake_core
    backend._networking = FakeNetworkingApi()
    _stub_create_dependencies(
        monkeypatch,
        backend,
        extra_env=[{"name": "CLAUDE_CODE_AUTH_MODE", "value": "access_token"}],
    )

    await backend.create("slack:C123:123.456", "claude-code", "claude-code")

    proxy_yaml = fake_core.created_configmaps[0][1]["data"]["proxy.yaml"]
    assert "anthropic-claude" in proxy_yaml
    assert "ANTHROPIC_API_KEY" not in proxy_yaml
    # Wrong-engine harness creds must not leak in.
    assert "openai-codex" not in proxy_yaml
    assert "OPENAI_API_KEY" not in proxy_yaml


@pytest.mark.asyncio
async def test_create_uses_brokered_creds_for_codex_oauth(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """codex + CODEX_AUTH_MODE=access_token must publish the brokered
    ``openai-codex`` credential plus the ``OPENAI_CODEX_ACCOUNT_ID`` header
    inject, and must NOT include ``OPENAI_API_KEY``."""
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    backend._core = fake_core
    backend._networking = FakeNetworkingApi()
    _stub_create_dependencies(
        monkeypatch,
        backend,
        extra_env=[{"name": "CODEX_AUTH_MODE", "value": "access_token"}],
        harness_cmd="codex-app-wrapper",
    )

    await backend.create("slack:C123:123.456", "codex", "codex")

    proxy_yaml = fake_core.created_configmaps[0][1]["data"]["proxy.yaml"]
    assert "openai-codex" in proxy_yaml
    assert "OPENAI_CODEX_ACCOUNT_ID" in proxy_yaml
    assert "OPENAI_API_KEY" not in proxy_yaml
    # Wrong-engine harness creds must not leak in.
    assert "anthropic-claude" not in proxy_yaml
    assert "ANTHROPIC_API_KEY" not in proxy_yaml


@pytest.mark.asyncio
async def test_create_uses_api_key_in_default_mode(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """With no AUTH_MODE set, each engine gets its ``api_key`` HttpSecret and
    no brokered credential — the existing behavior before OAuth modes."""
    # claude-code default
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    backend._core = fake_core
    backend._networking = FakeNetworkingApi()
    _stub_create_dependencies(monkeypatch, backend)

    await backend.create("slack:C123:123.456", "claude-code", "claude-code")

    proxy_yaml = fake_core.created_configmaps[0][1]["data"]["proxy.yaml"]
    assert "ANTHROPIC_API_KEY" in proxy_yaml
    assert "anthropic-claude" not in proxy_yaml
    # The other engine's creds must not leak in.
    assert "OPENAI_API_KEY" not in proxy_yaml
    assert "openai-codex" not in proxy_yaml


@pytest.mark.asyncio
async def test_create_isolates_codex_api_key_from_claude_sandbox(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A claude-code sandbox in api_key mode must not see OPENAI_API_KEY; a
    codex sandbox in api_key mode must not see ANTHROPIC_API_KEY. Each
    sandbox's iron-proxy holds only the creds its harness uses."""
    backend = KubernetesExecutorBackend()
    fake_core = FakeCoreApi()
    backend._core = fake_core
    backend._networking = FakeNetworkingApi()
    _stub_create_dependencies(monkeypatch, backend, harness_cmd="codex-app-wrapper")

    await backend.create("slack:C123:123.456", "codex", "codex")

    proxy_yaml = fake_core.created_configmaps[0][1]["data"]["proxy.yaml"]
    assert "OPENAI_API_KEY" in proxy_yaml
    assert "ANTHROPIC_API_KEY" not in proxy_yaml
