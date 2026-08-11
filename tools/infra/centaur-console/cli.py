"""CLI for centaur-console sandbox permission introspection."""

from __future__ import annotations

import hashlib
import json
import os
import sys
from pathlib import Path
from typing import Any

import typer
import yaml
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

load_dotenv()

app = typer.Typer(
    name="centaur-console",
    help="Inspect the current sandbox's centaur-console permissions",
)
console = Console()
skills_app = typer.Typer(
    name="centaur-skills",
    help="Discover builtin and Console-authored skills available to this agent",
    no_args_is_help=True,
)


def get_client(
    url: str | None = None,
    bearer_token: str | None = None,
):
    from .client import ConsoleClient

    return ConsoleClient(url=url, bearer_token=bearer_token)


@app.command("permissions")
def permissions(
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
):
    """Print the current sandbox's redacted permissions as JSON."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.sandbox_permissions()
    console.print_json(json.dumps(result, default=str))


@app.command("oauth-apps")
def oauth_apps(
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
):
    """Print enabled OAuth apps and their consent start URLs as JSON."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        result = client.sandbox_oauth_apps()
    console.print_json(json.dumps({"data": result}, default=str))


@app.command()
def health(
    url: str | None = typer.Option(None, "--url", help="centaur-console base URL"),
    bearer_token: str | None = typer.Option(
        None,
        "--bearer-token",
        help="Local/debug bearer token override",
        envvar="CENTAUR_CONSOLE_BEARER_TOKEN",
    ),
):
    """Assert the sandbox permissions endpoint is reachable and authorized."""
    with get_client(url=url, bearer_token=bearer_token) as client:
        payload = client.health()
    print(json.dumps(payload, indent=2, default=str))
    if not payload.get("ok"):
        raise typer.Exit(1)


def _builtin_skill_dirs() -> list[Path]:
    configured = os.getenv("WORKSPACE_DIR", "").strip()
    candidates = [Path(configured)] if configured else []
    candidates.extend([Path("/workspace"), Path.cwd()])
    result: list[Path] = []
    for root in candidates:
        skill_dir = root / ".agents" / "skills"
        if skill_dir.is_dir() and skill_dir not in result:
            result.append(skill_dir)
    return result


def _parse_builtin(path: Path) -> dict[str, Any] | None:
    try:
        content = path.read_text(encoding="utf-8")
    except OSError:
        return None
    if not content.startswith("---\n"):
        return None
    try:
        frontmatter, _body = content[4:].split("\n---", 1)
        metadata = yaml.safe_load(frontmatter)
    except (ValueError, yaml.YAMLError):
        return None
    if not isinstance(metadata, dict):
        return None
    name = str(metadata.get("name") or path.parent.name).strip()
    description = str(metadata.get("description") or "").strip()
    return {
        "ref": f"builtin:{path.parent.name}",
        "name": name,
        "description": description,
        "visibility": "builtin",
        "author": "deployment",
        "updated_at": None,
        "checksum": hashlib.sha256(content.encode()).hexdigest(),
        "content": content,
        "path": str(path),
    }


def _builtin_skills() -> list[dict[str, Any]]:
    skills: dict[str, dict[str, Any]] = {}
    for directory in _builtin_skill_dirs():
        for skill_file in sorted(directory.glob("*/SKILL.md")):
            parsed = _parse_builtin(skill_file)
            if parsed:
                skills[parsed["ref"]] = parsed
    return sorted(skills.values(), key=lambda item: str(item["name"]))


def _builtin_search(query: str, limit: int) -> list[dict[str, Any]]:
    terms = [term for term in query.lower().split() if term]

    def score(skill: dict[str, Any]) -> tuple[int, str]:
        name = str(skill.get("name") or "").lower()
        description = str(skill.get("description") or "").lower()
        content = str(skill.get("content") or "").lower()
        value = sum(8 for term in terms if term == name)
        value += sum(4 for term in terms if term in name)
        value += sum(2 for term in terms if term in description)
        value += sum(1 for term in terms if term in content)
        return value, name

    ranked = [(score(skill), skill) for skill in _builtin_skills()]
    ranked = [item for item in ranked if item[0][0] > 0]
    ranked.sort(key=lambda item: (-item[0][0], item[0][1]))
    return [skill for _score, skill in ranked[:limit]]


def _catalog_metadata(skill: dict[str, Any]) -> dict[str, Any]:
    return {key: value for key, value in skill.items() if key not in {"content", "path"}}


def _validate_output_flags(json_output: bool, markdown_output: bool) -> None:
    if json_output and markdown_output:
        raise typer.BadParameter("choose either --json or --markdown")


def _skill_identifier(skill: dict[str, Any]) -> str:
    return str(skill.get("id") or skill.get("ref") or "")


def _skill_document(skill: dict[str, Any]) -> str:
    return str(skill.get("document") or skill.get("content") or "")


def _merge_skill_results(*groups: list[dict[str, Any]], limit: int) -> list[dict[str, Any]]:
    merged: list[dict[str, Any]] = []
    seen: set[str] = set()
    offset = 0
    while len(merged) < limit:
        added = False
        for group in groups:
            if offset >= len(group):
                continue
            added = True
            skill = group[offset]
            ref = _skill_identifier(skill)
            if ref and ref not in seen:
                seen.add(ref)
                merged.append(skill)
                if len(merged) == limit:
                    break
        if not added:
            break
        offset += 1
    return merged


def _print_skill_results(
    results: list[dict[str, Any]],
    *,
    json_output: bool,
    markdown_output: bool,
) -> None:
    if json_output:
        print(json.dumps({"data": results}, indent=2, default=str))
        return
    if markdown_output:
        print("| ID | Name | Visibility | Description |")
        print("| --- | --- | --- | --- |")
        for skill in results:
            description = str(skill.get("description") or "").replace("|", "\\|")
            print(f"| {_skill_identifier(skill)} | {skill.get('name', '')} | {skill.get('visibility', '')} | {description} |")
        return

    table = Table(show_header=True, header_style="bold")
    table.add_column("ID", style="cyan", overflow="fold")
    table.add_column("Name", style="bold")
    table.add_column("Scope")
    table.add_column("Description", overflow="fold")
    for skill in results:
        table.add_row(
            _skill_identifier(skill),
            str(skill.get("name") or ""),
            str(skill.get("visibility") or ""),
            str(skill.get("description") or ""),
        )
    console.print(table)


@skills_app.command("search")
def skills_search(
    query: str = typer.Argument(..., help="Task or capability to find guidance for"),
    limit: int = typer.Option(10, "--limit", "-n", min=1, max=20),
    json_output: bool = typer.Option(False, "--json", help="Output JSON"),
    markdown_output: bool = typer.Option(False, "--markdown", help="Output a Markdown table"),
) -> None:
    """Search builtin and Console-authored skills relevant to a task."""
    _validate_output_flags(json_output, markdown_output)
    builtin = [_catalog_metadata(skill) for skill in _builtin_search(query, limit)]
    try:
        with get_client() as client:
            remote = client.skills_search(query, limit=limit)
    except RuntimeError as exc:
        remote = []
        print(f"warning: Console skill search unavailable: {exc}", file=sys.stderr)
    results = _merge_skill_results(builtin, remote, limit=limit)
    _print_skill_results(results, json_output=json_output, markdown_output=markdown_output)


@skills_app.command("list")
def skills_list(
    scope: str | None = typer.Option(None, "--scope", help="private, shared, or builtin"),
    limit: int = typer.Option(20, "--limit", "-n", min=1, max=20),
    json_output: bool = typer.Option(False, "--json", help="Output JSON"),
    markdown_output: bool = typer.Option(False, "--markdown", help="Output a Markdown table"),
) -> None:
    """List skills available to the current sandbox principal."""
    _validate_output_flags(json_output, markdown_output)
    normalized_scope = scope.strip().lower() if scope else None
    if normalized_scope not in {None, "private", "shared", "builtin"}:
        raise typer.BadParameter("scope must be private, shared, or builtin")
    builtin = (
        [_catalog_metadata(skill) for skill in _builtin_skills()[:limit]]
        if normalized_scope in {None, "builtin"}
        else []
    )
    remote: list[dict[str, Any]] = []
    if normalized_scope != "builtin":
        try:
            with get_client() as client:
                remote = client.skills_list(scope=normalized_scope, limit=limit)
        except RuntimeError as exc:
            if normalized_scope is not None:
                raise
            print(f"warning: Console skill catalog unavailable: {exc}", file=sys.stderr)
    results = _merge_skill_results(builtin, remote, limit=limit)
    _print_skill_results(results, json_output=json_output, markdown_output=markdown_output)


@skills_app.command("read")
def skills_read(
    ref: str = typer.Argument(..., help="Builtin reference or Console skill ID"),
    json_output: bool = typer.Option(False, "--json", help="Output JSON"),
    markdown_output: bool = typer.Option(False, "--markdown", help="Output the SKILL.md content"),
) -> None:
    """Read the complete current SKILL.md for one skill."""
    _validate_output_flags(json_output, markdown_output)
    if ref.startswith("builtin:"):
        result = next((skill for skill in _builtin_skills() if skill["ref"] == ref), None)
        if result is None:
            raise typer.BadParameter(f"unknown builtin skill: {ref}")
    elif ref.startswith("skl_"):
        with get_client() as client:
            result = client.skill_read(ref)
    else:
        raise typer.BadParameter("ref must be a builtin: reference or skl_ Console skill ID")

    if json_output:
        print(json.dumps({"data": result}, indent=2, default=str))
    else:
        document = _skill_document(result)
        print(document, end="" if document.endswith("\n") else "\n")


if __name__ == "__main__":
    app()
