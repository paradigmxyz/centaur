"""Sandbox backend registry — select and configure the active backend."""

from __future__ import annotations

from api.sandbox.base import SandboxBackend

_backend: SandboxBackend | None = None


def get_backend() -> SandboxBackend:
    """Get the configured sandbox backend. Auto-configures on first call."""
    global _backend
    if _backend is None:
        _backend = auto_configure()
    return _backend


def configure(backend: SandboxBackend) -> None:
    """Set the sandbox backend explicitly."""
    global _backend
    _backend = backend


def auto_configure() -> SandboxBackend:
    """Configure the Kubernetes sandbox backend."""
    import os

    controller = (os.getenv("KUBERNETES_SANDBOX_CONTROLLER") or "pod").strip().lower()
    if controller in {"agent-sandbox", "agentsandbox"}:
        from api.sandbox.kubernetes_agent_sandbox import KubernetesAgentSandboxBackend

        return KubernetesAgentSandboxBackend()

    from api.sandbox.kubernetes import KubernetesExecutorBackend

    return KubernetesExecutorBackend()
