"""Workflow: daily sandbox reaper + cost report.

A scheduled (cron) workflow that scans Centaur's ``sandbox_sessions`` table,
summarizes the fleet (state + harness breakdown, age buckets, long-idle and
terminal/"reapable" sessions, and a rough cost estimate), and posts a digest to
a Slack channel. Read-only — it reports; the actual reaping is handled by the
API's orphan reaper.

Configuration (all optional, via env):

- ``SANDBOX_REPORT_SLACK_CHANNEL`` — channel name/id to post to. The schedule is
  skipped entirely when this is unset, so the workflow is inert until an
  operator opts in.
- ``SANDBOX_REPORT_CRON`` — cron expression (default ``0 16 * * *``, 16:00 UTC).
- ``SANDBOX_REPORT_TZ`` — schedule timezone (default ``UTC``).
- ``SANDBOX_REPORT_ENABLED`` — set falsy to disable.
- ``SANDBOX_REPORT_IDLE_THRESHOLD_S`` — a non-terminal session whose
  ``updated_at`` is older than this counts as "long-idle" (default 6h).
- ``SANDBOX_POD_HOURLY_USD`` — per-pod hourly rate for the cost estimate
  (default 0.05). The figure is a rough estimate, labelled as such.
"""

from __future__ import annotations

import datetime as dt
import os
from collections import Counter
from dataclasses import dataclass, field
from typing import Any, Mapping, Sequence

from api.workflow_engine import Delivery, WorkflowContext

WORKFLOW_NAME = "sandbox_reaper_report"

# Terminal states linger in the table until reaped — surface them as cleanup
# candidates. (Active states: creating/running/idle/delivering/suspended.)
_TERMINAL_STATES = frozenset({"error", "stopped", "gone"})

_IDLE_THRESHOLD_S = max(int(os.getenv("SANDBOX_REPORT_IDLE_THRESHOLD_S", str(6 * 3600))), 0)
_POD_HOURLY_USD = max(float(os.getenv("SANDBOX_POD_HOURLY_USD", "0.05")), 0.0)
_MAX_LISTED = max(int(os.getenv("SANDBOX_REPORT_MAX_LISTED", "10")), 1)

SCHEDULE = {
    "cron": os.getenv("SANDBOX_REPORT_CRON", "0 16 * * *"),
    "timezone": os.getenv("SANDBOX_REPORT_TZ", "UTC"),
    "enabled": os.getenv("SANDBOX_REPORT_ENABLED", "true"),
    # When unset the schedule engine skips this workflow (no destination), so it
    # stays inert until an operator configures a channel.
    "slack_channel": os.getenv("SANDBOX_REPORT_SLACK_CHANNEL", ""),
}


@dataclass
class Input:
    delivery: Delivery = field(default_factory=Delivery)
    slack_channel: str = ""
    metadata: dict[str, Any] = field(default_factory=dict)


# ── pure helpers ─────────────────────────────────────────────────────────────


def _age_seconds(value: Any, now: dt.datetime) -> float | None:
    if not isinstance(value, dt.datetime):
        return None
    ts = value if value.tzinfo else value.replace(tzinfo=dt.timezone.utc)
    return max((now - ts).total_seconds(), 0.0)


def _bucket_for_age(age_s: float) -> str:
    hours = age_s / 3600.0
    if hours < 1:
        return "<1h"
    if hours < 6:
        return "1-6h"
    if hours < 24:
        return "6-24h"
    return ">24h"


def _aggregate(rows: Sequence[Mapping[str, Any]], now: dt.datetime) -> dict[str, Any]:
    """Summarize sandbox_sessions rows. Pure — no I/O."""
    by_state: Counter[str] = Counter()
    by_harness: Counter[str] = Counter()
    age_buckets: Counter[str] = Counter()
    est_cost = 0.0
    terminal: list[dict[str, Any]] = []
    long_idle: list[dict[str, Any]] = []

    for row in rows:
        state = str(row.get("state") or "unknown")
        harness = str(row.get("harness") or "unknown")
        by_state[state] += 1
        by_harness[harness] += 1
        is_terminal = state in _TERMINAL_STATES

        age_s = _age_seconds(row.get("started_at"), now)
        idle_s = _age_seconds(row.get("updated_at"), now)

        entry = {
            "sandbox_id": str(row.get("sandbox_id") or "")[:12],
            "harness": harness,
            "state": state,
            "age_hours": round((age_s or 0) / 3600.0, 1),
            "idle_hours": round((idle_s or 0) / 3600.0, 1),
        }

        if is_terminal:
            terminal.append(entry)
            continue

        # Active session: count toward live fleet age/cost.
        if age_s is not None:
            age_buckets[_bucket_for_age(age_s)] += 1
            est_cost += (age_s / 3600.0) * _POD_HOURLY_USD
        if idle_s is not None and idle_s >= _IDLE_THRESHOLD_S:
            long_idle.append(entry)

    long_idle.sort(key=lambda e: e["idle_hours"], reverse=True)
    terminal.sort(key=lambda e: e["age_hours"], reverse=True)

    active_total = sum(v for s, v in by_state.items() if s not in _TERMINAL_STATES)
    return {
        "total": sum(by_state.values()),
        "active_total": active_total,
        "terminal_total": len(terminal),
        "long_idle_total": len(long_idle),
        "by_state": dict(by_state),
        "by_harness": dict(by_harness),
        "age_buckets": {k: age_buckets[k] for k in ("<1h", "1-6h", "6-24h", ">24h") if age_buckets[k]},
        "est_cost_usd": round(est_cost, 2),
        "idle_threshold_hours": round(_IDLE_THRESHOLD_S / 3600.0, 1),
        "pod_hourly_usd": _POD_HOURLY_USD,
        "long_idle": long_idle[:_MAX_LISTED],
        "terminal": terminal[:_MAX_LISTED],
        "generated_at": now.isoformat(),
    }


def _fmt_counter(counter: Mapping[str, int]) -> str:
    if not counter:
        return "none"
    return ", ".join(f"{k}: {v}" for k, v in sorted(counter.items(), key=lambda kv: (-kv[1], kv[0])))


def _render_report(snapshot: Mapping[str, Any]) -> str:
    lines = [
        "*🧹 Sandbox fleet report*",
        f"• Active sandboxes: *{snapshot['active_total']}*  "
        f"(total rows incl. terminal: {snapshot['total']})",
        f"• By state: {_fmt_counter(snapshot['by_state'])}",
        f"• By harness: {_fmt_counter(snapshot['by_harness'])}",
    ]
    if snapshot["age_buckets"]:
        lines.append(f"• Active age: {_fmt_counter(snapshot['age_buckets'])}")
    lines.append(
        f"• Estimated active spend so far: *~${snapshot['est_cost_usd']:.2f}* "
        f"(@ ${snapshot['pod_hourly_usd']:.3f}/pod-hr, rough)"
    )

    long_idle = snapshot.get("long_idle") or []
    if long_idle:
        lines.append(
            f"• ⏳ Long-idle (>{snapshot['idle_threshold_hours']:.0f}h idle): "
            f"*{snapshot['long_idle_total']}*"
        )
        for e in long_idle:
            lines.append(
                f"    - `{e['sandbox_id']}` {e['harness']}/{e['state']} "
                f"— idle {e['idle_hours']:.1f}h, age {e['age_hours']:.1f}h"
            )

    terminal = snapshot.get("terminal") or []
    if terminal:
        lines.append(f"• 🪦 Terminal/reapable rows: *{snapshot['terminal_total']}*")
        for e in terminal:
            lines.append(
                f"    - `{e['sandbox_id']}` {e['harness']}/{e['state']} "
                f"— age {e['age_hours']:.1f}h"
            )

    if not long_idle and not terminal:
        lines.append("• ✅ No long-idle or terminal sandboxes — fleet looks clean.")

    return "\n".join(lines)


def _resolve_channel(inp: Input) -> str:
    delivery_channel = inp.delivery.channel if isinstance(inp.delivery, Delivery) else None
    return (
        (delivery_channel or "")
        or (inp.slack_channel or "")
        or os.getenv("SANDBOX_REPORT_SLACK_CHANNEL", "")
    ).strip().lstrip("#")


# ── data source ──────────────────────────────────────────────────────────────


async def _scan_sandboxes() -> dict[str, Any]:
    """Snapshot + aggregate sandbox_sessions. Wrapped in a checkpointed step."""
    from api.agent import _get_pool

    pool = _get_pool()
    rows = await pool.fetch(
        "SELECT thread_key, sandbox_id, harness, engine, state, started_at, updated_at "
        "FROM sandbox_sessions"
    )
    now = dt.datetime.now(dt.timezone.utc)
    return _aggregate([dict(r) for r in rows], now)


# ── handler ──────────────────────────────────────────────────────────────────


async def handler(inp: Input, ctx: WorkflowContext) -> dict[str, Any]:
    channel = _resolve_channel(inp)
    if not channel:
        ctx.log("sandbox_report_no_channel")
        return {"posted": False, "reason": "no_channel"}

    snapshot = await ctx.step("scan_sandboxes", _scan_sandboxes)
    ctx.log(
        "sandbox_report_snapshot",
        active=snapshot["active_total"],
        terminal=snapshot["terminal_total"],
        long_idle=snapshot["long_idle_total"],
        est_cost_usd=snapshot["est_cost_usd"],
    )
    await ctx.post_to_slack(channel, _render_report(snapshot))
    return {
        "posted": True,
        "channel": channel,
        "active_total": snapshot["active_total"],
        "terminal_total": snapshot["terminal_total"],
        "long_idle_total": snapshot["long_idle_total"],
        "est_cost_usd": snapshot["est_cost_usd"],
    }
