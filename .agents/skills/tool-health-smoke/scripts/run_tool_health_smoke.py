#!/usr/bin/env python3
"""Run health checks for all live Centaur tool CLIs."""

from __future__ import annotations

import argparse
import asyncio
from dataclasses import asdict, dataclass
import json
import shutil
import subprocess
import sys
import time
from typing import Any


@dataclass
class ToolResult:
    tool: str
    status: str
    seconds: float
    error: str | None = None
    detail: str | None = None
    returncode: int | None = None
    output: dict[str, Any] | None = None


def load_tools() -> list[str]:
    if shutil.which("centaur-tools") is None:
        raise RuntimeError("centaur-tools is not installed in PATH")

    result = subprocess.run(
        ["centaur-tools", "json"],
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        message = result.stderr or result.stdout or "centaur-tools json failed"
        raise RuntimeError(message)

    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"centaur-tools json returned invalid JSON: {exc}") from exc

    tools: list[str] = []
    for item in payload:
        if isinstance(item, dict) and isinstance(item.get("name"), str):
            tools.append(item["name"])
    return sorted(set(tools))


def parse_csv(value: str | None) -> set[str]:
    if not value:
        return set()
    return {part.strip() for part in value.split(",") if part.strip()}


def pick_detail(payload: dict[str, Any]) -> str:
    details = payload.get("details")
    if not isinstance(details, dict):
        return json.dumps(details, default=str) if details is not None else ""

    preferred: list[str] = []
    for key, value in details.items():
        if isinstance(value, (dict, list)):
            continue
        key_l = str(key).lower()
        if key_l in {"status", "ready", "count", "records_checked", "records_seen"}:
            preferred.append(f"{key}={value}")
        elif key_l.endswith(("_checked", "_count", "_results", "_seen")):
            preferred.append(f"{key}={value}")
    if not preferred:
        for key, value in details.items():
            if isinstance(value, (dict, list)):
                continue
            preferred.append(f"{key}={value}")
            if len(preferred) >= 3:
                break
    return ", ".join(preferred) if preferred else "health ok"


async def run_one(tool: str, timeout: float) -> ToolResult:
    start = time.monotonic()
    try:
        proc = await asyncio.create_subprocess_exec(
            tool,
            "health",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
    except FileNotFoundError:
        return ToolResult(tool=tool, status="FAIL", seconds=0.0, error="CLI not found")

    try:
        stdout_b, stderr_b = await asyncio.wait_for(proc.communicate(), timeout=timeout)
    except asyncio.TimeoutError:
        proc.kill()
        await proc.communicate()
        return ToolResult(
            tool=tool,
            status="FAIL",
            seconds=time.monotonic() - start,
            error=f"timed out after {timeout:g}s",
        )

    seconds = time.monotonic() - start
    stdout = stdout_b.decode(errors="replace")
    stderr = stderr_b.decode(errors="replace")

    payload: Any | None = None
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as exc:
        if proc.returncode != 0:
            return ToolResult(
                tool=tool,
                status="FAIL",
                seconds=seconds,
                error=stderr or stdout or f"exit {proc.returncode}",
                returncode=proc.returncode,
            )
        return ToolResult(
            tool=tool,
            status="FAIL",
            seconds=seconds,
            error=f"invalid health JSON: {exc}",
            returncode=proc.returncode,
        )

    if proc.returncode != 0:
        error = None
        if isinstance(payload, dict):
            error = payload.get("error")
        return ToolResult(
            tool=tool,
            status="FAIL",
            seconds=seconds,
            error=str(error or stderr or f"exit {proc.returncode}"),
            returncode=proc.returncode,
            output=payload if isinstance(payload, dict) else None,
        )

    if not isinstance(payload, dict) or "ok" not in payload:
        return ToolResult(
            tool=tool,
            status="FAIL",
            seconds=seconds,
            error="health output missing ok field",
            returncode=proc.returncode,
            output=payload if isinstance(payload, dict) else None,
        )

    ok = bool(payload.get("ok"))
    return ToolResult(
        tool=tool,
        status="PASS" if ok else "FAIL",
        seconds=seconds,
        error=None if ok else str(payload.get("error") or "health returned ok=false"),
        detail=pick_detail(payload) if ok else None,
        returncode=proc.returncode,
        output=payload,
    )


async def run_all(
    tools: list[str], timeout: float, concurrency: int
) -> list[ToolResult]:
    semaphore = asyncio.Semaphore(max(1, concurrency))

    async def guarded(tool: str) -> ToolResult:
        async with semaphore:
            return await run_one(tool, timeout)

    return await asyncio.gather(*(guarded(tool) for tool in tools))


def render_report(results: list[ToolResult], filtered: bool) -> str:
    failures = [row for row in results if row.status == "FAIL"]
    passes = [row for row in results if row.status == "PASS"]

    if not results:
        overall = "PARTIAL"
        reason = "no tools discovered"
    elif failures:
        overall = "FAIL"
        reason = f"{len(failures)} of {len(results)} tool health checks failed"
    elif filtered:
        overall = "PARTIAL"
        reason = f"{len(passes)} filtered tool health checks passed"
    else:
        overall = "PASS"
        reason = f"{len(passes)} tool health checks passed"

    lines = [f"Overall: {overall} - {reason}", "", "*Tool Health*"]
    lines.append(f"- *Discovered:* {len(results)} checked")
    lines.append(f"- *Passed:* {len(passes)}")
    lines.append(f"- *Failed:* {len(failures)}")

    if failures:
        lines.append("")
        lines.append("*Failures*")
        for row in failures:
            lines.append(f"- *{row.tool}:* FAIL - {row.error or 'unknown error'}")

    if passes:
        lines.append("")
        lines.append("*Passes*")
        for row in passes:
            detail = f" - {row.detail}" if row.detail else ""
            lines.append(f"- *{row.tool}:* PASS{detail}")

    return "\n".join(lines)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--timeout", type=float, default=60.0, help="Seconds per tool health command"
    )
    parser.add_argument(
        "--concurrency", type=int, default=4, help="Concurrent health commands"
    )
    parser.add_argument("--only", help="Comma-separated tool names to include")
    parser.add_argument("--exclude", help="Comma-separated tool names to exclude")
    parser.add_argument(
        "--json", action="store_true", help="Emit machine-readable JSON"
    )
    args = parser.parse_args(argv)

    try:
        tools = load_tools()
    except Exception as exc:
        if args.json:
            print(
                json.dumps(
                    {"ok": False, "error": str(exc), "results": []}, indent=2
                )
            )
        else:
            print(f"Overall: FAIL - {exc}")
        return 1

    only = parse_csv(args.only)
    exclude = parse_csv(args.exclude)
    filtered = bool(only or exclude)
    if only:
        tools = [tool for tool in tools if tool in only]
    if exclude:
        tools = [tool for tool in tools if tool not in exclude]

    results = asyncio.run(run_all(tools, args.timeout, args.concurrency))
    results = sorted(results, key=lambda row: row.tool)
    failures = [row for row in results if row.status == "FAIL"]

    if args.json:
        print(
            json.dumps(
                {
                    "ok": not failures and bool(results),
                    "checked": len(results),
                    "passed": len(results) - len(failures),
                    "failed": len(failures),
                    "results": [asdict(row) for row in results],
                },
                indent=2,
                default=str,
            )
        )
    else:
        print(render_report(results, filtered))

    return 1 if failures or not results else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
