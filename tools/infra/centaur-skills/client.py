"""Clients for repository and sandbox-scoped Console skill catalogs."""

from __future__ import annotations

import os
import re
from pathlib import Path
from typing import Any
from urllib.parse import quote

import httpx
import yaml

SANDBOX_SKILLS_PATH = "/api/v1/sandbox/skills"
REPOSITORY_SOURCE = "repository"
CONSOLE_SOURCE = "console"


class SkillsClient:
    """Read and author Console skills for the current sandbox principal."""

    def __init__(
        self,
        url: str | None = None,
        bearer_token: str | None = None,
        timeout: float = 30.0,
        transport: httpx.BaseTransport | None = None,
    ):
        self._url = url
        self._bearer_token = bearer_token
        self.timeout = timeout
        self._transport = transport
        self._client: httpx.Client | None = None

    @property
    def base_url(self) -> str:
        # Non-secret endpoint config. Sandboxes receive this from api-rs.
        url = (
            (self._url or os.getenv("CENTAUR_CONSOLE_URL", "http://centaur-console:3000"))  # noqa: TID251
            .strip()
            .rstrip("/")
        )
        if url and not url.startswith(("http://", "https://")):
            url = f"http://{url}"
        return url

    def _headers(self) -> dict[str, str]:
        headers = {"Accept": "application/json"}
        # iron-proxy injects this entitlement in sandboxes. The environment
        # override exists only for local debugging.
        bearer = (self._bearer_token or os.getenv("CENTAUR_CONSOLE_BEARER_TOKEN", "")).strip()  # noqa: TID251
        if bearer:
            headers["Authorization"] = f"Bearer {bearer}"
        return headers

    @property
    def client(self) -> httpx.Client:
        if self._client is None:
            self._client = httpx.Client(
                base_url=self.base_url,
                headers=self._headers(),
                timeout=self.timeout,
                transport=self._transport,
            )
        return self._client

    def list(self, scope: str | None = None, limit: int = 20) -> list[dict[str, Any]]:
        """List Console skills visible to the current sandbox principal."""
        params: dict[str, str | int] = {"limit": limit}
        if scope:
            params["scope"] = scope
        result = self._request(SANDBOX_SKILLS_PATH, params=params)
        if not isinstance(result, list):
            raise RuntimeError("centaur-skills response did not include a data array")
        return result

    def search(self, query: str, limit: int = 10) -> list[dict[str, Any]]:
        """Search Console skills visible to the current sandbox principal."""
        result = self._request(
            f"{SANDBOX_SKILLS_PATH}/search",
            params={"q": query, "limit": limit},
        )
        if not isinstance(result, list):
            raise RuntimeError("centaur-skills response did not include a data array")
        return result

    def read(self, identifier: str) -> dict[str, Any]:
        """Read one visible Console skill by exact name or OID."""
        result = self._request(f"{SANDBOX_SKILLS_PATH}/{quote(identifier, safe='')}")
        if not isinstance(result, dict):
            raise RuntimeError("centaur-skills response did not include a data object")
        return result

    def create(self, name: str, description: str, instructions: str) -> dict[str, Any]:
        """Create a shared Console skill owned by the current Console user."""
        result = self._request(
            SANDBOX_SKILLS_PATH,
            method="POST",
            json={
                "data": {
                    "name": name,
                    "description": description,
                    "instructions": instructions,
                }
            },
        )
        if not isinstance(result, dict):
            raise RuntimeError("centaur-skills response did not include a data object")
        return result

    def edit(
        self,
        identifier: str,
        name: str | None = None,
        description: str | None = None,
        instructions: str | None = None,
        lock_version: int | None = None,
    ) -> dict[str, Any]:
        """Edit an owned or editable Console skill by OID."""
        attributes: dict[str, str | int] = {}
        if name is not None:
            attributes["name"] = name
        if description is not None:
            attributes["description"] = description
        if instructions is not None:
            attributes["instructions"] = instructions
        if not attributes:
            raise ValueError("at least one skill field must be provided")
        if lock_version is not None:
            attributes["lock_version"] = lock_version

        result = self._request(
            f"{SANDBOX_SKILLS_PATH}/{quote(identifier, safe='')}",
            method="PATCH",
            json={"data": attributes},
        )
        if not isinstance(result, dict):
            raise RuntimeError("centaur-skills response did not include a data object")
        return result

    def delete(self, identifier: str) -> None:
        """Archive an owned Console skill by OID."""
        self._request(
            f"{SANDBOX_SKILLS_PATH}/{quote(identifier, safe='')}",
            method="DELETE",
        )

    def list_editors(self, identifier: str) -> dict[str, Any]:
        """List editors for a visible Console skill by exact name or OID."""
        result = self._request(f"{SANDBOX_SKILLS_PATH}/{quote(identifier, safe='')}/editors")
        if not isinstance(result, dict):
            raise RuntimeError("centaur-skills response did not include a data object")
        return result

    def add_editor(self, identifier: str, user: str) -> dict[str, Any]:
        """Add an editor to an owned skill by exact email or Console user OID."""
        return self._manage_editor(identifier, user, method="POST")

    def remove_editor(self, identifier: str, user: str) -> dict[str, Any]:
        """Remove an editor from an owned skill by exact email or Console user OID."""
        return self._manage_editor(identifier, user, method="DELETE")

    def _manage_editor(self, identifier: str, user: str, *, method: str) -> dict[str, Any]:
        normalized_user = user.strip()
        if not normalized_user:
            raise ValueError("user email or OID must not be empty")

        result = self._request(
            f"{SANDBOX_SKILLS_PATH}/{quote(identifier, safe='')}/editors",
            method=method,
            json={"data": {"user": normalized_user}},
        )
        if not isinstance(result, dict):
            raise RuntimeError("centaur-skills response did not include a data object")
        return result

    def _request(
        self,
        path: str,
        method: str = "GET",
        params: dict[str, str | int] | None = None,
        json: dict[str, Any] | None = None,
    ) -> dict[str, Any] | list[dict[str, Any]] | None:
        try:
            response = self.client.request(method, path, params=params, json=json)
            response.raise_for_status()
        except httpx.HTTPStatusError as exc:
            detail = _response_error_detail(exc.response)
            raise RuntimeError(f"centaur-skills request failed: {detail}") from exc
        except httpx.RequestError as exc:
            raise RuntimeError(f"centaur-skills request failed: {exc}") from exc

        if not response.content:
            return None

        payload = response.json()
        data = payload.get("data")
        if not isinstance(data, (dict, list)):
            raise RuntimeError("centaur-skills response did not include data")
        return data

    def close(self) -> None:
        if self._client:
            self._client.close()
            self._client = None

    def __enter__(self) -> SkillsClient:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()


class RepositorySkillsCatalog:
    """Read skills installed in the workspace's merged `.agents/skills` tree."""

    def __init__(self, skills_dir: Path | None = None):
        self.skills_dir = skills_dir or _workspace_skills_dir()

    def list(self, limit: int | None = None) -> list[dict[str, Any]]:
        """List repository and overlay skills available in the workspace."""
        skills = sorted(self._skills().values(), key=lambda skill: skill["name"])
        if limit is not None:
            return skills[:limit]
        return skills

    def search(self, query: str, limit: int = 10) -> list[dict[str, Any]]:
        """Search local skill names and descriptions."""
        normalized_query = _normalize_search_text(query)
        if not normalized_query:
            return []

        query_terms = normalized_query.split()
        matches: list[tuple[tuple[int, int, str], dict[str, Any]]] = []
        for skill in self._skills().values():
            name = _normalize_search_text(str(skill["name"]))
            description = _normalize_search_text(str(skill.get("description", "")))
            haystack = f"{name} {description}"
            haystack_terms = set(haystack.split())
            matched_terms = sum(term in haystack_terms for term in query_terms)
            if not matched_terms:
                continue

            score = matched_terms
            if normalized_query == name:
                score += 100
            elif normalized_query in name:
                score += 50
            elif normalized_query in f"{name} {description}":
                score += 25

            matches.append(((-score, -matched_terms, str(skill["name"])), skill))

        matches.sort(key=lambda match: match[0])
        return [skill for _, skill in matches[:limit]]

    def read(self, identifier: str) -> dict[str, Any] | None:
        """Read a repository skill by exact name or `repo:` identifier."""
        name = identifier.removeprefix("repo:")
        return self._skills(include_document=True).get(name)

    def _skills(self, *, include_document: bool = False) -> dict[str, dict[str, Any]]:
        if not self.skills_dir.is_dir():
            return {}

        skills: dict[str, dict[str, Any]] = {}
        root = self.skills_dir.resolve()
        for entry in sorted(self.skills_dir.iterdir(), key=lambda path: path.name):
            document_path = entry / "SKILL.md"
            if not document_path.is_file():
                continue
            try:
                resolved_document = document_path.resolve()
                resolved_document.relative_to(root)
                document = document_path.read_text(encoding="utf-8")
            except (OSError, UnicodeError, ValueError):
                continue

            metadata = _skill_frontmatter(document)
            name = metadata.get("name")
            if not name:
                continue

            skill: dict[str, Any] = {
                "id": f"repo:{name}",
                "name": name,
                "description": metadata.get("description", ""),
                "source": REPOSITORY_SOURCE,
            }
            if include_document:
                skill["document"] = document
            skills[name] = skill
        return skills


class SkillsCatalog(SkillsClient):
    """Merge workspace skills with principal-visible Console skills."""

    def __init__(
        self,
        repository: RepositorySkillsCatalog | None = None,
        url: str | None = None,
        bearer_token: str | None = None,
        timeout: float = 30.0,
        transport: httpx.BaseTransport | None = None,
    ):
        super().__init__(
            url=url,
            bearer_token=bearer_token,
            timeout=timeout,
            transport=transport,
        )
        self.repository = repository or RepositorySkillsCatalog()

    def list(self, scope: str | None = None, limit: int = 20) -> list[dict[str, Any]]:
        """List the merged catalog, or one source when scope is explicit."""
        if scope == REPOSITORY_SOURCE:
            return self.repository.list(limit=limit)

        console_skills = [
            _with_source(skill, CONSOLE_SOURCE) for skill in super().list(scope=scope, limit=limit)
        ]
        if scope in {"private", "shared"}:
            return console_skills

        return _merge_skills(self.repository.list(), console_skills, limit=limit)

    def search(self, query: str, limit: int = 10) -> list[dict[str, Any]]:
        """Search both catalogs while preserving source-distinct results."""
        repository_skills = self.repository.search(query, limit=limit)
        console_skills = [
            _with_source(skill, CONSOLE_SOURCE) for skill in super().search(query, limit=limit)
        ]
        return _merge_skills(repository_skills, console_skills, limit=limit)

    def read(self, identifier: str) -> dict[str, Any]:
        """Read a repository skill first by name, or a Console skill by OID."""
        if not identifier.startswith("skl_"):
            repository_skill = self.repository.read(identifier)
            if repository_skill is not None:
                return repository_skill
            if identifier.startswith("repo:"):
                raise RuntimeError(f"repository skill not found: {identifier}")
        return _with_source(super().read(identifier), CONSOLE_SOURCE)


def _workspace_skills_dir() -> Path:
    for directory in (Path.cwd(), *Path.cwd().parents):
        candidate = directory / ".agents" / "skills"
        if candidate.is_dir():
            return candidate
    return Path.cwd() / ".agents" / "skills"


def _skill_frontmatter(document: str) -> dict[str, str]:
    match = re.match(
        r"\A---[ \t]*\r?\n(.*?)\r?\n---[ \t]*(?:\r?\n|\Z)",
        document,
        flags=re.DOTALL,
    )
    if match is None:
        return {}

    try:
        metadata = yaml.safe_load(match.group(1))
    except yaml.YAMLError:
        return {}
    if not isinstance(metadata, dict):
        return {}

    return {
        key: metadata[key] for key in ("name", "description") if isinstance(metadata.get(key), str)
    }


def _normalize_search_text(value: str) -> str:
    return " ".join(re.findall(r"[a-z0-9]+", value.lower()))


def _with_source(skill: dict[str, Any], source: str) -> dict[str, Any]:
    return {**skill, "source": source}


def _merge_skills(
    repository: list[dict[str, Any]],
    console: list[dict[str, Any]],
    *,
    limit: int,
) -> list[dict[str, Any]]:
    merged: list[dict[str, Any]] = []
    for index in range(max(len(repository), len(console))):
        for skills in (repository, console):
            if index >= len(skills):
                continue
            skill = skills[index]
            merged.append(skill)
            if len(merged) == limit:
                return merged
    return merged


def _response_error_detail(response: httpx.Response) -> str:
    try:
        body = response.json()
    except ValueError:
        body = response.text
    return f"HTTP {response.status_code}: {body}"


def _client() -> SkillsCatalog:
    return SkillsCatalog()
