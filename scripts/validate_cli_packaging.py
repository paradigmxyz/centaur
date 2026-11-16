#!/usr/bin/env python3
"""Validate tool CLI entry points and Hatch wheel package mappings."""

from __future__ import annotations

from pathlib import Path
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]
TOOLS_DIR = ROOT / "tools"


def _is_ignored(path: Path) -> bool:
    return any(part in {".venv", "__pycache__", ".git"} for part in path.parts)


def _nearest_pyproject(path: Path) -> Path | None:
    current = path.parent
    while current != ROOT and current != current.parent:
        pyproject = current / "pyproject.toml"
        if pyproject.exists():
            return pyproject
        current = current.parent
    return None


def _script_source_exists(
    project_dir: Path,
    module: str,
    wheel: dict,
) -> bool:
    package = module.removesuffix(".cli")
    package_path = Path(*package.split("."))
    packages = wheel.get("packages") or []
    sources = wheel.get("sources") or {}

    if package in packages:
        return (project_dir / package_path / "cli.py").is_file()

    if "." in packages and sources.get("") == package:
        return (project_dir / "cli.py").is_file()

    if "." in packages and sources.get(".") == package:
        return (project_dir / "cli.py").is_file()

    return False


def validate() -> list[str]:
    errors: list[str] = []

    cli_projects: dict[Path, list[Path]] = {}
    for cli_file in sorted(TOOLS_DIR.rglob("cli.py")):
        if _is_ignored(cli_file):
            continue
        pyproject = _nearest_pyproject(cli_file)
        if pyproject is None:
            errors.append(f"{cli_file.relative_to(ROOT)}: missing owning pyproject.toml")
            continue
        cli_projects.setdefault(pyproject, []).append(cli_file)

    for pyproject, cli_files in sorted(cli_projects.items()):
        try:
            data = tomllib.loads(pyproject.read_text())
        except tomllib.TOMLDecodeError as exc:
            errors.append(f"{pyproject.relative_to(ROOT)}: invalid TOML: {exc}")
            continue

        project = data.get("project") or {}
        scripts = project.get("scripts") or {}
        wheel = (
            ((data.get("tool") or {}).get("hatch") or {})
            .get("build", {})
            .get("targets", {})
            .get("wheel", {})
        )

        label = str(pyproject.relative_to(ROOT))
        if not isinstance(scripts, dict) or not scripts:
            errors.append(f"{label}: missing [project.scripts] for {len(cli_files)} CLI file(s)")
            continue
        if not isinstance(wheel, dict) or not wheel:
            errors.append(f"{label}: missing [tool.hatch.build.targets.wheel]")
            continue
        if "only-include" in wheel:
            errors.append(f"{label}: use wheel packages/sources, not only-include")

        for script_name, target in sorted(scripts.items()):
            print(f"checking {script_name}: {label} -> {target}")
            if not isinstance(target, str):
                errors.append(f"{label}: script {script_name!r} target must be a string")
                continue
            if target.startswith("cli:"):
                errors.append(f"{label}: script {script_name!r} uses top-level {target!r}")
                continue
            if not target.endswith(".cli:app"):
                errors.append(
                    f"{label}: script {script_name!r} must target '<package>.cli:app', got {target!r}"
                )
                continue
            module = target.rsplit(":", 1)[0]
            if not _script_source_exists(pyproject.parent, module, wheel):
                package = module.removesuffix(".cli")
                errors.append(
                    f"{label}: script {script_name!r} target {target!r} does not match "
                    f"wheel packages/sources for package {package!r}"
                )

    return errors


def main() -> int:
    errors = validate()
    if errors:
        print("CLI packaging validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("CLI packaging validation passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
