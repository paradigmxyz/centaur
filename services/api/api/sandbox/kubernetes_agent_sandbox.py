from __future__ import annotations

import os
from typing import Any

from kubernetes_asyncio import client

from api.sandbox.base import SandboxSession
from api.sandbox.kubernetes import (
    KubernetesExecutorBackend,
    _namespace,
    _prompt_secret_name,
)

_AGENT_SANDBOX_GROUP = "agents.x-k8s.io"
_AGENT_SANDBOX_VERSION = "v1alpha1"
_AGENT_SANDBOX_PLURAL = "sandboxes"


def _state_volume_enabled() -> bool:
    value = (os.getenv("KUBERNETES_SANDBOX_STATE_VOLUME_ENABLED") or "").strip().lower()
    return value in {"1", "true", "yes", "on"}


def _state_volume_size() -> str:
    return (os.getenv("KUBERNETES_SANDBOX_STATE_VOLUME_SIZE") or "10Gi").strip()


def _state_volume_storage_class_name() -> str:
    return (os.getenv("KUBERNETES_SANDBOX_STATE_VOLUME_STORAGE_CLASS") or "").strip()


def _state_pvc_name(sandbox_id: str) -> str:
    return f"state-{sandbox_id}"


class KubernetesAgentSandboxBackend(KubernetesExecutorBackend):
    """Runs agent sandboxes through the Agent Sandbox controller."""

    def __init__(self) -> None:
        super().__init__()
        self._custom: client.CustomObjectsApi | None = None

    async def _ensure_clients(self) -> None:
        await super()._ensure_clients()
        if self._custom is None:
            self._custom = client.CustomObjectsApi(api_client=self._core_api().api_client)

    def _custom_api(self) -> client.CustomObjectsApi:
        if self._custom is None:
            raise RuntimeError("kubernetes custom objects client not initialized")
        return self._custom

    async def _agent_sandbox_replicas(self, sandbox_id: str) -> int | None:
        try:
            sandbox = await self._custom_api().get_namespaced_custom_object(
                _AGENT_SANDBOX_GROUP,
                _AGENT_SANDBOX_VERSION,
                _namespace(),
                _AGENT_SANDBOX_PLURAL,
                sandbox_id,
            )
        except Exception as exc:
            if self._is_not_found(exc):
                return None
            raise
        spec = sandbox.get("spec", {}) if isinstance(sandbox, dict) else {}
        raw = spec.get("replicas", 1)
        try:
            return int(raw)
        except (TypeError, ValueError):
            return 1

    async def _delete_sandbox(self, sandbox_id: str) -> None:
        try:
            await self._custom_api().delete_namespaced_custom_object(
                _AGENT_SANDBOX_GROUP,
                _AGENT_SANDBOX_VERSION,
                _namespace(),
                _AGENT_SANDBOX_PLURAL,
                sandbox_id,
            )
        except Exception as exc:
            if not self._is_not_found(exc):
                raise

    async def _delete_state_pvc(self, sandbox_id: str) -> None:
        try:
            await self._core_api().delete_namespaced_persistent_volume_claim(
                _state_pvc_name(sandbox_id),
                _namespace(),
            )
        except Exception as exc:
            if not self._is_not_found(exc):
                raise

    def _configure_workload_volumes(
        self,
        volume_mounts: list[dict[str, Any]],
        volumes: list[dict[str, Any]],  # noqa: ARG002
    ) -> None:
        if _state_volume_enabled():
            volume_mounts.append({"name": "state", "mountPath": "/home/agent/state"})

    async def _delete_existing_workload(self, pod_name: str) -> None:
        await self._delete_pod(pod_name)
        await self._delete_sandbox(pod_name)

    async def _create_workload(self, pod_spec: dict[str, Any]) -> None:
        sandbox_id = pod_spec["metadata"]["name"]
        spec: dict[str, Any] = {
            "replicas": 1,
            "service": False,
            "shutdownPolicy": "Retain",
            "podTemplate": {
                "metadata": {
                    "labels": pod_spec["metadata"].get("labels", {}),
                    "annotations": pod_spec["metadata"].get("annotations", {}),
                },
                "spec": pod_spec["spec"],
            },
        }
        if _state_volume_enabled():
            pvc_spec: dict[str, Any] = {
                "accessModes": ["ReadWriteOnce"],
                "resources": {"requests": {"storage": _state_volume_size()}},
            }
            storage_class = _state_volume_storage_class_name()
            if storage_class:
                pvc_spec["storageClassName"] = storage_class
            spec["volumeClaimTemplates"] = [
                {
                    "metadata": {"name": "state"},
                    "spec": pvc_spec,
                }
            ]

        body: dict[str, Any] = {
            "apiVersion": f"{_AGENT_SANDBOX_GROUP}/{_AGENT_SANDBOX_VERSION}",
            "kind": "Sandbox",
            "metadata": {
                "name": sandbox_id,
                "labels": pod_spec["metadata"].get("labels", {}),
                "annotations": pod_spec["metadata"].get("annotations", {}),
            },
            "spec": spec,
        }
        await self._custom_api().create_namespaced_custom_object(
            _AGENT_SANDBOX_GROUP,
            _AGENT_SANDBOX_VERSION,
            _namespace(),
            _AGENT_SANDBOX_PLURAL,
            body,
        )

    async def _cleanup_workload_after_create_error(self, pod_name: str) -> None:
        await self._delete_sandbox(pod_name)

    async def pause_by_id(self, sandbox_id: str) -> None:
        await self._ensure_clients()
        await self.close_streams(
            SandboxSession(sandbox_id=sandbox_id, thread_key="", harness="", engine="")
        )
        await self._custom_api().patch_namespaced_custom_object(
            _AGENT_SANDBOX_GROUP,
            _AGENT_SANDBOX_VERSION,
            _namespace(),
            _AGENT_SANDBOX_PLURAL,
            sandbox_id,
            {"spec": {"replicas": 0}},
            _content_type="application/merge-patch+json",
        )

    async def resume_by_id(self, sandbox_id: str) -> None:
        await self._ensure_clients()
        await self._custom_api().patch_namespaced_custom_object(
            _AGENT_SANDBOX_GROUP,
            _AGENT_SANDBOX_VERSION,
            _namespace(),
            _AGENT_SANDBOX_PLURAL,
            sandbox_id,
            {"spec": {"replicas": 1}},
            _content_type="application/merge-patch+json",
        )
        await self._wait_ready(sandbox_id)

    async def status_by_id(self, sandbox_id: str) -> str:
        await self._ensure_clients()
        replicas = await self._agent_sandbox_replicas(sandbox_id)
        if replicas is None:
            return "gone"
        try:
            pod = await self._core_api().read_namespaced_pod(sandbox_id, _namespace())
        except Exception as exc:
            if self._is_not_found(exc):
                return "suspended" if replicas == 0 else "created"
            raise
        if (
            getattr(getattr(pod, "metadata", None), "deletion_timestamp", None)
            is not None
        ):
            return "suspended" if replicas == 0 else "created"
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
        await self._delete_sandbox(sandbox_id)
        await self._delete_state_pvc(sandbox_id)
        await self._delete_prompt_secret(_prompt_secret_name(sandbox_id))
        await self._delete_proxy_resources(sandbox_id)
