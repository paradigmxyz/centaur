"""Registry-backed client for Machine Payments Protocol services.

The client deliberately knows nothing about wallets or payment credentials. It
makes ordinary HTTP requests; Centaur's egress proxy handles supported MPP 402
challenges without exposing signing keys to the sandbox.
"""

from __future__ import annotations

import fnmatch
import json
import os
import re
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import quote, unquote, urljoin, urlsplit

import httpx

DEFAULT_REGISTRY_URL = "https://mpp.dev/api/services"
DEFAULT_CACHE_TTL_SECONDS = 15 * 60
DEFAULT_MAX_STALE_SECONDS = 24 * 60 * 60
DEFAULT_MAX_RESPONSE_BYTES = 10 * 1024 * 1024
CACHE_SCHEMA_VERSION = 1
SENSITIVE_HEADER_NAMES = {
    "authorization",
    "cookie",
    "host",
    "proxy-authorization",
    "set-cookie",
    "transfer-encoding",
}
PATH_PARAMETER = re.compile(r":([A-Za-z_][A-Za-z0-9_]*)|\{([A-Za-z_][A-Za-z0-9_]*)\}")


class MppCatalogError(RuntimeError):
    """The registry could not provide a usable catalog."""


class MppPolicyError(RuntimeError):
    """A registered route is disabled by the local MPP policy."""


class MppRequestError(RuntimeError):
    """A registered MPP service request failed."""


@dataclass(frozen=True)
class CatalogSnapshot:
    catalog: dict[str, Any]
    fetched_at: float
    etag: str | None = None
    last_modified: str | None = None
    from_cache: bool = False
    stale: bool = False

    def metadata(self, now: float) -> dict[str, Any]:
        return {
            "fetched_at": self.fetched_at,
            "age_seconds": max(0, int(now - self.fetched_at)),
            "from_cache": self.from_cache,
            "stale": self.stale,
        }


@dataclass(frozen=True)
class PolicyDecision:
    allowed: bool
    reason: str


class MppClient:
    """Discover and call registered MPP services through Centaur's proxy."""

    def __init__(
        self,
        *,
        registry_url: str | None = None,
        cache_path: str | Path | None = None,
        cache_ttl_seconds: int | None = None,
        max_stale_seconds: int | None = None,
        default_methods: list[str] | None = None,
        policy_rules: list[dict[str, Any]] | None = None,
        timeout: float = 30,
        max_response_bytes: int = DEFAULT_MAX_RESPONSE_BYTES,
        now: Any = time.time,
    ) -> None:
        self.registry_url = registry_url or _config_env("MPP_REGISTRY_URL", DEFAULT_REGISTRY_URL)
        self.cache_path = Path(
            cache_path
            or _config_env(
                "MPP_REGISTRY_CACHE_PATH",
                str(Path.home() / ".cache" / "centaur" / "mpp" / "registry-v1.json"),
            )
        )
        self.cache_ttl_seconds = (
            cache_ttl_seconds
            if cache_ttl_seconds is not None
            else _positive_int_env("MPP_REGISTRY_CACHE_TTL_SECONDS", DEFAULT_CACHE_TTL_SECONDS)
        )
        self.max_stale_seconds = (
            max_stale_seconds
            if max_stale_seconds is not None
            else _positive_int_env("MPP_REGISTRY_MAX_STALE_SECONDS", DEFAULT_MAX_STALE_SECONDS)
        )
        self.default_methods = {
            method.upper()
            for method in (
                default_methods
                if default_methods is not None
                else _csv_env("MPP_DEFAULT_METHODS", ["GET"])
            )
        }
        self.policy_rules = (
            policy_rules if policy_rules is not None else _json_rules_env("MPP_POLICY_RULES")
        )
        self.timeout = timeout
        self.max_response_bytes = max_response_bytes
        self._now = now
        self._catalog_http: httpx.Client | None = None
        self._service_http: httpx.Client | None = None

        _validate_https_url(self.registry_url, "MPP registry URL")
        if self.cache_ttl_seconds > self.max_stale_seconds:
            raise ValueError("MPP registry cache TTL cannot exceed max stale duration")
        if not self.default_methods:
            raise ValueError("MPP default methods cannot be empty")
        _validate_policy_rules(self.policy_rules)

    @property
    def catalog_http(self) -> httpx.Client:
        if self._catalog_http is None:
            self._catalog_http = httpx.Client(timeout=self.timeout, follow_redirects=False)
        return self._catalog_http

    @property
    def service_http(self) -> httpx.Client:
        if self._service_http is None:
            self._service_http = httpx.Client(timeout=self.timeout, follow_redirects=False)
        return self._service_http

    def list_services(
        self,
        query: str | None = None,
        category: str | None = None,
        tag: str | None = None,
        limit: int = 20,
    ) -> dict[str, Any]:
        """List registry services and route availability under the local policy."""
        _validate_limit(limit)
        snapshot = self._catalog()
        services = self._filter_services(
            snapshot.catalog["services"],
            query=query,
            category=category,
            tag=tag,
            limit=limit,
        )
        return {
            "services": [self._service_summary(service) for service in services],
            "cache": snapshot.metadata(self._now()),
        }

    def search_services(
        self,
        query: str,
        category: str | None = None,
        tag: str | None = None,
        limit: int = 20,
    ) -> dict[str, Any]:
        """Search service ids, names, descriptions, categories, and tags."""
        if not query.strip():
            raise ValueError("MPP service search query cannot be empty")
        return self.list_services(query=query, category=category, tag=tag, limit=limit)

    def show_service(self, service: str) -> dict[str, Any]:
        """Show one service with an availability decision for every endpoint."""
        snapshot = self._catalog()
        record = self._find_service(snapshot.catalog["services"], service)
        result = dict(record)
        result["endpoints"] = [
            {
                **endpoint,
                "availability": self._endpoint_availability(record, endpoint),
            }
            for endpoint in record.get("endpoints", [])
        ]
        result["cache"] = snapshot.metadata(self._now())
        return result

    def request(
        self,
        service: str,
        method: str,
        path: str,
        path_params: dict[str, str] | None = None,
        query: dict[str, Any] | None = None,
        body: dict[str, Any] | list[Any] | None = None,
    ) -> dict[str, Any]:
        """Call one exact registered route through Centaur's egress proxy."""
        snapshot = self._catalog()
        record = self._find_service(snapshot.catalog["services"], service)
        endpoint = self._find_endpoint(record, method, path)
        availability = self._endpoint_availability(record, endpoint)
        if not availability["executable"]:
            raise MppPolicyError(availability["reason"])

        request_method = endpoint["method"].upper()
        if request_method in {"GET", "HEAD"} and body is not None:
            raise ValueError(f"{request_method} requests cannot include a JSON body")
        resolved_path = _resolve_path(endpoint["path"], path_params or {})
        base_url = _service_url(record)
        url = urljoin(f"{base_url.rstrip('/')}/", resolved_path.lstrip("/"))
        _validate_service_destination(url, base_url)

        try:
            response = self.service_http.request(
                request_method,
                url,
                params=query,
                json=body,
                headers={"Accept": "application/json"},
            )
        except httpx.RequestError as exc:
            raise MppRequestError(f"MPP service request failed: {exc.__class__.__name__}") from exc

        if response.is_redirect:
            raise MppRequestError("MPP service redirects are not allowed")
        if response.status_code == 402:
            raise MppRequestError(
                "MPP payment was not authorized; transparent payment may be disabled or denied"
            )
        try:
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            raise MppRequestError(f"MPP service returned HTTP {exc.response.status_code}") from exc

        content = response.content
        if len(content) > self.max_response_bytes:
            raise MppRequestError("MPP service response exceeded the configured size limit")
        content_type = response.headers.get("content-type", "").lower()
        if "json" in content_type:
            try:
                payload: Any = response.json()
            except ValueError as exc:
                raise MppRequestError("MPP service returned invalid JSON") from exc
        else:
            payload = response.text

        receipt = response.headers.get("Payment-Receipt")
        return {
            "service": record["id"],
            "method": request_method,
            "path": resolved_path,
            "status": response.status_code,
            "payment_receipt_present": receipt is not None,
            "data": payload,
        }

    def health(self) -> dict[str, Any]:
        """Refresh the registry and report cache and policy readiness."""
        snapshot = self._catalog()
        return {
            "ok": True,
            "registry_url": self.registry_url,
            "service_count": len(snapshot.catalog["services"]),
            "cache": snapshot.metadata(self._now()),
            "default_methods": sorted(self.default_methods),
        }

    def close(self) -> None:
        if self._catalog_http is not None:
            self._catalog_http.close()
            self._catalog_http = None
        if self._service_http is not None:
            self._service_http.close()
            self._service_http = None

    def __enter__(self) -> MppClient:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def _catalog(self) -> CatalogSnapshot:
        now = self._now()
        cached = self._read_cache()
        if cached is not None and now - cached.fetched_at <= self.cache_ttl_seconds:
            return CatalogSnapshot(
                catalog=cached.catalog,
                fetched_at=cached.fetched_at,
                etag=cached.etag,
                last_modified=cached.last_modified,
                from_cache=True,
                stale=False,
            )

        try:
            refreshed = self._refresh_catalog(cached, now)
            self._write_cache(refreshed)
            return refreshed
        except MppCatalogError:
            if cached is None or now - cached.fetched_at > self.max_stale_seconds:
                raise
            stale = CatalogSnapshot(
                catalog=cached.catalog,
                fetched_at=cached.fetched_at,
                etag=cached.etag,
                last_modified=cached.last_modified,
                from_cache=True,
                stale=True,
            )
            return stale

    def _refresh_catalog(self, cached: CatalogSnapshot | None, now: float) -> CatalogSnapshot:
        headers: dict[str, str] = {"Accept": "application/json"}
        if cached and cached.etag:
            headers["If-None-Match"] = cached.etag
        if cached and cached.last_modified:
            headers["If-Modified-Since"] = cached.last_modified
        try:
            response = self.catalog_http.get(self.registry_url, headers=headers)
        except httpx.RequestError as exc:
            raise MppCatalogError(f"MPP registry request failed: {exc.__class__.__name__}") from exc
        if response.status_code == 304 and cached is not None:
            return CatalogSnapshot(
                catalog=cached.catalog,
                fetched_at=now,
                etag=cached.etag,
                last_modified=cached.last_modified,
            )
        if response.is_redirect:
            raise MppCatalogError("MPP registry redirects are not allowed")
        try:
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            raise MppCatalogError(f"MPP registry returned HTTP {exc.response.status_code}") from exc
        try:
            catalog = response.json()
        except ValueError as exc:
            raise MppCatalogError("MPP registry returned invalid JSON") from exc
        _validate_catalog(catalog)
        return CatalogSnapshot(
            catalog=catalog,
            fetched_at=now,
            etag=response.headers.get("etag"),
            last_modified=response.headers.get("last-modified"),
        )

    def _read_cache(self) -> CatalogSnapshot | None:
        try:
            payload = json.loads(self.cache_path.read_text())
            if payload.get("schema_version") != CACHE_SCHEMA_VERSION:
                return None
            catalog = payload["catalog"]
            _validate_catalog(catalog)
            return CatalogSnapshot(
                catalog=catalog,
                fetched_at=float(payload["fetched_at"]),
                etag=payload.get("etag"),
                last_modified=payload.get("last_modified"),
            )
        except (FileNotFoundError, KeyError, TypeError, ValueError, json.JSONDecodeError):
            return None

    def _write_cache(self, snapshot: CatalogSnapshot) -> None:
        payload = {
            "schema_version": CACHE_SCHEMA_VERSION,
            "fetched_at": snapshot.fetched_at,
            "etag": snapshot.etag,
            "last_modified": snapshot.last_modified,
            "catalog": snapshot.catalog,
        }
        self.cache_path.parent.mkdir(parents=True, exist_ok=True)
        temporary_path: Path | None = None
        try:
            with tempfile.NamedTemporaryFile(
                "w",
                encoding="utf-8",
                dir=self.cache_path.parent,
                prefix=f".{self.cache_path.name}.",
                delete=False,
            ) as temporary:
                temporary_path = Path(temporary.name)
                json.dump(payload, temporary, separators=(",", ":"), ensure_ascii=False)
                temporary.flush()
                os.fsync(temporary.fileno())
            temporary_path.replace(self.cache_path)
            directory = os.open(self.cache_path.parent, os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            if temporary_path is not None:
                temporary_path.unlink(missing_ok=True)

    def _filter_services(
        self,
        services: list[dict[str, Any]],
        *,
        query: str | None,
        category: str | None,
        tag: str | None,
        limit: int,
    ) -> list[dict[str, Any]]:
        query_folded = query.casefold() if query else None
        category_folded = category.casefold() if category else None
        tag_folded = tag.casefold() if tag else None
        matches: list[dict[str, Any]] = []
        for service in services:
            haystack = [
                service.get("id", ""),
                service.get("name", ""),
                service.get("description", ""),
                *service.get("categories", []),
                *service.get("tags", []),
            ]
            if query_folded and not any(
                query_folded in str(value).casefold() for value in haystack
            ):
                continue
            if category_folded and category_folded not in {
                str(value).casefold() for value in service.get("categories", [])
            }:
                continue
            if tag_folded and tag_folded not in {
                str(value).casefold() for value in service.get("tags", [])
            }:
                continue
            matches.append(service)
            if len(matches) == limit:
                break
        return matches

    def _service_summary(self, service: dict[str, Any]) -> dict[str, Any]:
        endpoints = service.get("endpoints", [])
        endpoint_availability = [
            {
                "method": endpoint["method"].upper(),
                "path": endpoint["path"],
                **self._endpoint_availability(service, endpoint),
            }
            for endpoint in endpoints
        ]
        executable = sum(1 for availability in endpoint_availability if availability["executable"])
        unavailable_reasons = sorted(
            {
                availability["reason"]
                for availability in endpoint_availability
                if not availability["executable"]
            }
        )
        if not endpoints:
            unavailable_reasons = ["service has no registered endpoints"]
        return {
            "id": service["id"],
            "name": service.get("name"),
            "description": service.get("description"),
            "service_url": service.get("serviceUrl") or service.get("url"),
            "categories": service.get("categories", []),
            "tags": service.get("tags", []),
            "status": service.get("status"),
            "endpoints": len(endpoints),
            "executable_endpoints": executable,
            "available": executable > 0,
            "unavailable_reasons": unavailable_reasons,
        }

    def _find_service(self, services: list[dict[str, Any]], value: str) -> dict[str, Any]:
        folded = value.casefold()
        exact_ids = [service for service in services if service["id"].casefold() == folded]
        if exact_ids:
            return exact_ids[0]
        names = [
            service for service in services if str(service.get("name", "")).casefold() == folded
        ]
        if len(names) == 1:
            return names[0]
        if len(names) > 1:
            raise ValueError(f"MPP service name {value!r} is ambiguous; use a service id")
        raise ValueError(f"MPP service {value!r} was not found")

    def _find_endpoint(self, service: dict[str, Any], method: str, path: str) -> dict[str, Any]:
        normalized_method = method.upper()
        matches = [
            endpoint
            for endpoint in service.get("endpoints", [])
            if endpoint["method"].upper() == normalized_method and endpoint["path"] == path
        ]
        if not matches:
            raise ValueError(
                f"{normalized_method} {path!r} is not registered for MPP service {service['id']!r}"
            )
        return matches[0]

    def _endpoint_availability(
        self, service: dict[str, Any], endpoint: dict[str, Any]
    ) -> dict[str, Any]:
        status = service.get("status")
        if status not in {None, "active"}:
            return {"executable": False, "reason": f"service status is {status!r}"}
        decision = self._policy_decision(service, endpoint)
        payment = endpoint.get("payment") or {}
        intent = payment.get("intent")
        method = payment.get("method")
        if intent != "charge":
            return {"executable": False, "reason": f"unsupported payment intent {intent!r}"}
        if method != "tempo":
            return {"executable": False, "reason": f"unsupported payment method {method!r}"}
        return {"executable": decision.allowed, "reason": decision.reason}

    def _policy_decision(self, service: dict[str, Any], endpoint: dict[str, Any]) -> PolicyDecision:
        method = endpoint["method"].upper()
        matched_allow = False
        for rule in self.policy_rules:
            if not _rule_matches(rule, service, endpoint):
                continue
            if rule["effect"] == "deny":
                return PolicyDecision(False, "denied by operator policy")
            matched_allow = True
        if matched_allow:
            return PolicyDecision(True, "allowed by operator policy")
        if method in self.default_methods:
            return PolicyDecision(True, f"{method} is enabled by default")
        return PolicyDecision(False, f"{method} requires an operator policy rule")


def _client() -> MppClient:
    return MppClient()


def _positive_int_env(name: str, default: int) -> int:
    value = _config_env(name)
    if value is None:
        return default
    try:
        parsed = int(value)
    except ValueError as exc:
        raise ValueError(f"{name} must be an integer") from exc
    if parsed <= 0:
        raise ValueError(f"{name} must be positive")
    return parsed


def _csv_env(name: str, default: list[str]) -> list[str]:
    value = _config_env(name)
    if value is None:
        return default
    return [part.strip() for part in value.split(",") if part.strip()]


def _json_rules_env(name: str) -> list[dict[str, Any]]:
    value = _config_env(name)
    if not value:
        return []
    try:
        payload = json.loads(value)
    except json.JSONDecodeError as exc:
        raise ValueError(f"{name} must contain valid JSON") from exc
    if not isinstance(payload, list):
        raise ValueError(f"{name} must contain a JSON list")
    return payload


def _config_env(name: str, default: str | None = None) -> str | None:
    """Read non-secret operator configuration propagated into the sandbox."""
    return os.environ.get(name, default)  # noqa: TID251


def _validate_limit(limit: int) -> None:
    if limit < 1 or limit > 100:
        raise ValueError("MPP service limit must be between 1 and 100")


def _validate_https_url(value: str, label: str) -> None:
    if not isinstance(value, str):
        raise ValueError(f"{label} must be an absolute HTTPS URL without credentials")
    parsed = urlsplit(value)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
        raise ValueError(f"{label} must be an absolute HTTPS URL without credentials")


def _validate_catalog(payload: Any) -> None:
    if not isinstance(payload, dict) or not isinstance(payload.get("services"), list):
        raise MppCatalogError("MPP registry returned an invalid services list")
    service_ids: set[str] = set()
    for service in payload["services"]:
        if (
            not isinstance(service, dict)
            or not isinstance(service.get("id"), str)
            or not service["id"]
            or service["id"] in service_ids
        ):
            raise MppCatalogError("MPP registry returned an invalid services list")
        service_ids.add(service["id"])
        service_url = service.get("serviceUrl") or service.get("url")
        try:
            _validate_https_url(service_url, "MPP service URL")
        except (TypeError, ValueError) as exc:
            raise MppCatalogError(
                f"MPP registry service {service['id']!r} has an invalid URL"
            ) from exc
        endpoints = service.get("endpoints")
        if not isinstance(endpoints, list):
            raise MppCatalogError(f"MPP registry service {service['id']!r} has invalid endpoints")
        for endpoint in endpoints:
            if (
                not isinstance(endpoint, dict)
                or not isinstance(endpoint.get("method"), str)
                or not endpoint["method"]
                or not endpoint["method"].isalpha()
                or not isinstance(endpoint.get("path"), str)
                or not endpoint["path"].startswith("/")
                or _has_unsafe_path_segments(endpoint["path"])
            ):
                raise MppCatalogError(
                    f"MPP registry service {service['id']!r} has an invalid endpoint"
                )


def _validate_policy_rules(rules: list[dict[str, Any]]) -> None:
    if not isinstance(rules, list):
        raise ValueError("MPP policy rules must be a list")
    supported = {"effect", "service", "category", "realm", "methods", "path"}
    for index, rule in enumerate(rules):
        if not isinstance(rule, dict) or rule.get("effect") not in {"allow", "deny"}:
            raise ValueError(f"MPP policy rule {index} must have effect 'allow' or 'deny'")
        unknown = set(rule) - supported
        if unknown:
            raise ValueError(f"MPP policy rule {index} has unsupported fields: {sorted(unknown)}")
        methods = rule.get("methods")
        if methods is not None and (
            not isinstance(methods, list) or not all(isinstance(value, str) for value in methods)
        ):
            raise ValueError(f"MPP policy rule {index} methods must be a list of strings")


def _rule_matches(rule: dict[str, Any], service: dict[str, Any], endpoint: dict[str, Any]) -> bool:
    service_pattern = rule.get("service")
    if service_pattern and not fnmatch.fnmatchcase(service["id"], service_pattern):
        return False
    category = rule.get("category")
    if category and category.casefold() not in {
        str(value).casefold() for value in service.get("categories", [])
    }:
        return False
    realm_pattern = rule.get("realm")
    realm = service.get("realm") or urlsplit(_service_url(service)).hostname or ""
    if realm_pattern and not fnmatch.fnmatchcase(realm, realm_pattern):
        return False
    methods = rule.get("methods")
    if methods and endpoint["method"].upper() not in {value.upper() for value in methods}:
        return False
    path_pattern = rule.get("path")
    return not path_pattern or fnmatch.fnmatchcase(endpoint["path"], path_pattern)


def _service_url(service: dict[str, Any]) -> str:
    value = service.get("serviceUrl") or service.get("url")
    _validate_https_url(value, "MPP service URL")
    return value


def _resolve_path(template: str, path_params: dict[str, str]) -> str:
    names = {match.group(1) or match.group(2) for match in PATH_PARAMETER.finditer(template)}
    missing = names - set(path_params)
    extra = set(path_params) - names
    if missing:
        raise ValueError(f"missing MPP path parameters: {sorted(missing)}")
    if extra:
        raise ValueError(f"unexpected MPP path parameters: {sorted(extra)}")

    def replace(match: re.Match[str]) -> str:
        name = match.group(1) or match.group(2)
        value = str(path_params[name])
        if not value or "/" in unquote(value) or value in {".", ".."}:
            raise ValueError(f"invalid MPP path parameter {name!r}")
        return quote(value, safe="")

    resolved = PATH_PARAMETER.sub(replace, template)
    decoded_segments = [unquote(segment) for segment in resolved.split("/")]
    if not resolved.startswith("/") or any(segment in {".", ".."} for segment in decoded_segments):
        raise ValueError("invalid MPP request path")
    return resolved


def _validate_service_destination(url: str, base_url: str) -> None:
    target = urlsplit(url)
    base = urlsplit(base_url)
    if target.scheme != "https" or target.hostname != base.hostname or target.port != base.port:
        raise ValueError("MPP request destination does not match the registered service")


def _has_unsafe_path_segments(path: str) -> bool:
    for segment in path.split("/"):
        decoded = unquote(segment)
        if decoded in {".", ".."} or "/" in decoded or "\\" in decoded:
            return True
    return False
