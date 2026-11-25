"""Module-level singleton for the active secret backend.

Server mode (default): ``StubBackend`` — returns key names as values so the
firewall can replace them with real secrets in-flight.  ``EnvBackend`` is
**banned** in server mode; see the ruff per-file-ignores in pyproject.toml.

CLI mode: call ``configure()`` explicitly with ``EnvBackend`` before using
``secret()``.
"""

from __future__ import annotations

from centaur_sdk.backends.base import SecretBackend

_backend: SecretBackend | None = None


def configure(backend: SecretBackend) -> None:
    """Set the active secret backend."""
    global _backend
    _backend = backend


def auto_configure() -> SecretBackend:
    """Configure the default backend for server mode.

    Returns a ``StubBackend`` that yields key names as placeholder values.
    The firewall replaces these with real secrets in outbound HTTPS headers.

    **Do not use ``EnvBackend`` here.** Real secrets must never be resolvable
    inside the API process.  See README.md § Security Architecture, invariant S1.
    """
    from centaur_sdk.backends.stub import StubBackend

    backend = StubBackend()
    configure(backend)
    return backend


def get_backend() -> SecretBackend:
    """Return the active backend, auto-configuring on first call if needed."""
    global _backend
    if _backend is None:
        auto_configure()
    assert _backend is not None
    return _backend
