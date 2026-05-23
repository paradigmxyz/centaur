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
from api.sandbox.registry import auto_configure


class FakeCoreApi:
    def __init__(self) -> None:
        self.deleted_secrets: list[tuple[str, str]] = []
        self.deleted_pods: list[tuple[str, str, int]] = []
        self.deleted_services: list[tuple[str, str]] = []
        self.deleted_configmaps: list[tuple[str, str]] = []
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
            return self.pods_to_read.pop(0)
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


def test_container_env_passes_laminar_otel_config(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(
        "LMNR_BASE_URL",
        "http://stg-laminar-app-server.stg-laminar.svc.cluster.local:8000",
    )
    monkeypatch.setenv("LMNR_PROJECT_API_KEY", "lmnr-key")
    monkeypatch.setenv("CODEX_OTEL_ENVIRONMENT", "staging")

    env = sandbox_container_env("thread-key", "sandbox-id", "firewall.internal")
    env_map = dict(item.split("=", 1) for item in env)

    assert (
        env_map["LMNR_BASE_URL"]
        == "http://stg-laminar-app-server.stg-laminar.svc.cluster.local:8000"
    )
    assert env_map["LMNR_PROJECT_API_KEY"] == "lmnr-key"
    assert env_map["CODEX_OTEL_ENVIRONMENT"] == "staging"


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
                    "value": "localhost,127.0.0.1,api.internal,laminar.internal",
                },
                {
                    "name": "no_proxy",
                    "value": "localhost,127.0.0.1,api.internal,laminar.internal",
                },
                {
                    "name": "LMNR_BASE_URL",
                    "value": "http://laminar.internal:8000",
                },
            ]
        ),
    )

    env = sandbox_container_env("thread-key", "sandbox-id", "firewall.internal")
    env_map = dict(item.split("=", 1) for item in env)

    assert env_map["NO_PROXY"] == "localhost,127.0.0.1,api.internal,laminar.internal"
    assert env_map["no_proxy"] == "localhost,127.0.0.1,api.internal,laminar.internal"
    assert env_map["LMNR_BASE_URL"] == "http://laminar.internal:8000"
    assert len([item for item in env if item.startswith("NO_PROXY=")]) == 1
    assert len([item for item in env if item.startswith("no_proxy=")]) == 1


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
    backend = auto_configure()

    assert isinstance(backend, KubernetesExecutorBackend)


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
