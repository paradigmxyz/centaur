"""Generate performance graphs from block metrics.

Graphs follow the Centaur visual signature: 16:9, 200 DPI on save, Okabe-Ito
categorical palette, sentence-case takeaway titles, no top/right spines, and
horizontal-only gridlines.
"""

from __future__ import annotations

from pathlib import Path

import matplotlib
import numpy as np
import pandas as pd
from matplotlib.ticker import PercentFormatter

matplotlib.use("Agg")

import matplotlib.pyplot as plt

from .parser import BlockMetrics

# Two-tone palette for stacked bars: protagonist (Centaur primary) + warm
# accent (Okabe-Ito vermilion). Stable across themes.
_OKABE_ITO = ["#0072B2", "#D55E00", "#009E73", "#CC79A7", "#F0E442", "#56B4E9", "#E69F00"]
_EXEC_COLOR = _OKABE_ITO[0]  # blue
_STATE_COLOR = _OKABE_ITO[1]  # vermilion
_TREND_COLOR = _OKABE_ITO[1]  # also vermilion for the dashed trend line


def _subplots(figsize: tuple[float, float]) -> tuple[plt.Figure, plt.Axes]:
    fig, ax = plt.subplots(figsize=figsize)
    ax.spines["top"].set_visible(False)
    ax.spines["right"].set_visible(False)
    ax.grid(axis="y", color="#E5E7EB", linewidth=0.8)
    return fig, ax


def _subtitle_title(ax: plt.Axes, title: str, subtitle: str | None = None) -> None:
    ax.set_title(title, loc="left", fontsize=15, fontweight=700, pad=16)
    if subtitle:
        ax.text(0, 1.02, subtitle, transform=ax.transAxes, ha="left", va="bottom", fontsize=10)


def _save(fig: plt.Figure, output_path: Path) -> None:
    fig.tight_layout()
    fig.savefig(output_path, dpi=200, bbox_inches="tight")
    plt.close(fig)


def metrics_to_dataframe(blocks: list[BlockMetrics]) -> pd.DataFrame:
    """Convert block metrics to a pandas DataFrame."""
    data = []
    for b in blocks:
        state_root_ms = b.state_root_elapsed_ms or 0.0
        execution_ms = max(0.0, b.elapsed_ms - state_root_ms)
        state_root_pct = (state_root_ms / b.elapsed_ms * 100) if b.elapsed_ms > 0 else 0
        execution_pct = 100.0 - state_root_pct

        data.append(
            {
                "timestamp": b.timestamp,
                "block_number": b.block_number,
                "txs": b.txs,
                "gas_used_mgas": b.gas_used_mgas,
                "gas_throughput_ggas_s": b.gas_throughput_mgas_s / 1000.0,
                "gas_limit_mgas": b.gas_limit_mgas,
                "full_pct": b.full_pct,
                "base_fee_gwei": b.base_fee_gwei,
                "blobs": b.blobs,
                "elapsed_ms": b.elapsed_ms,
                "state_root_ms": state_root_ms,
                "execution_ms": execution_ms,
                "state_root_pct": state_root_pct,
                "execution_pct": execution_pct,
            }
        )
    return pd.DataFrame(data)


def _filter_or_raise(df: pd.DataFrame, min_gas_mgas: float) -> pd.DataFrame:
    filtered = df[df["gas_used_mgas"] > min_gas_mgas].copy()
    if filtered.empty:
        raise ValueError(f"No blocks with gas > {min_gas_mgas} Mgas")
    return filtered


def _suffix_subtitle(min_gas_mgas: float, title_suffix: str) -> str:
    parts: list[str] = []
    if min_gas_mgas > 0:
        parts.append(f"blocks > {min_gas_mgas:.0f} Mgas")
    if title_suffix:
        parts.append(title_suffix)
    return " · ".join(parts)


def plot_gas_throughput(
    df: pd.DataFrame, output_path: Path, min_gas_mgas: float = 0.0, title_suffix: str = ""
) -> Path:
    """Plot gas throughput over time."""
    filtered = _filter_or_raise(df, min_gas_mgas)

    fig, ax = _subplots(figsize=(8.0, 4.5))
    ax.plot(
        filtered["block_number"],
        filtered["gas_throughput_ggas_s"],
        color=_EXEC_COLOR,
        linewidth=1.0,
        alpha=0.9,
        zorder=10,
    )
    ax.scatter(
        filtered["block_number"],
        filtered["gas_throughput_ggas_s"],
        color=_EXEC_COLOR,
        s=14,
        alpha=0.55,
        edgecolors="none",
        zorder=11,
    )

    avg_throughput = filtered["gas_throughput_ggas_s"].mean()
    ax.axhline(
        y=avg_throughput,
        color=_TREND_COLOR,
        linestyle="--",
        linewidth=1.2,
        zorder=5,
    )
    ax.text(
        ax.get_xlim()[1],
        avg_throughput,
        f"  avg {avg_throughput:.2f} Ggas/s",
        ha="left",
        va="center",
        color=_TREND_COLOR,
        fontsize=9,
        fontweight=600,
    )

    ax.set_xlabel("Block number")
    ax.set_ylabel("Gas throughput (Ggas/s)")
    _subtitle_title(
        ax,
        f"Average gas throughput: {avg_throughput:.2f} Ggas/s",
        subtitle=_suffix_subtitle(min_gas_mgas, title_suffix) or None,
    )
    _save(fig, output_path)
    return output_path


def plot_latency_breakdown(
    df: pd.DataFrame, output_path: Path, min_gas_mgas: float = 0.0, title_suffix: str = ""
) -> Path:
    """Plot latency breakdown (state root vs execution) as stacked bar chart."""
    filtered = _filter_or_raise(df, min_gas_mgas)

    fig, ax = _subplots(figsize=(8.0, 4.5))
    width = 0.8
    x = np.arange(len(filtered))

    ax.bar(
        x,
        filtered["execution_ms"].values,
        width,
        label="Execution",
        color=_EXEC_COLOR,
        edgecolor="none",
    )
    ax.bar(
        x,
        filtered["state_root_ms"].values,
        width,
        bottom=filtered["execution_ms"].values,
        label="State root",
        color=_STATE_COLOR,
        edgecolor="none",
    )

    step = max(1, len(filtered) // 10)
    ax.set_xticks(x[::step])
    ax.set_xticklabels(filtered["block_number"].values[::step], rotation=45, ha="right")
    ax.set_xlabel("Block number")
    ax.set_ylabel("Latency (ms)")
    ax.legend(loc="upper right", frameon=False)

    _subtitle_title(
        ax,
        "Block latency breakdown — execution vs state root",
        subtitle=_suffix_subtitle(min_gas_mgas, title_suffix) or None,
    )
    _save(fig, output_path)
    return output_path


def plot_latency_percentage(
    df: pd.DataFrame, output_path: Path, min_gas_mgas: float = 0.0, title_suffix: str = ""
) -> Path:
    """Plot state root vs execution as percentage of total latency."""
    filtered = _filter_or_raise(df, min_gas_mgas)

    fig, ax = _subplots(figsize=(8.0, 4.5))
    width = 0.8
    x = np.arange(len(filtered))

    ax.bar(
        x,
        filtered["execution_pct"].values,
        width,
        label="Execution %",
        color=_EXEC_COLOR,
        edgecolor="none",
    )
    ax.bar(
        x,
        filtered["state_root_pct"].values,
        width,
        bottom=filtered["execution_pct"].values,
        label="State root %",
        color=_STATE_COLOR,
        edgecolor="none",
    )

    step = max(1, len(filtered) // 10)
    ax.set_xticks(x[::step])
    ax.set_xticklabels(filtered["block_number"].values[::step], rotation=45, ha="right")

    avg_execution = filtered["execution_pct"].mean()
    avg_state_root = filtered["state_root_pct"].mean()

    ax.set_xlabel("Block number")
    ax.set_ylabel("Percentage of total latency")
    ax.set_ylim(0, 100)
    ax.yaxis.set_major_formatter(PercentFormatter(xmax=100, decimals=0))
    ax.legend(loc="upper right", frameon=False)

    _subtitle_title(
        ax,
        f"Execution dominated at {avg_execution:.1f}% of latency",
        subtitle=(
            f"State root averaged {avg_state_root:.1f}%"
            + (
                f" · {_suffix_subtitle(min_gas_mgas, title_suffix)}"
                if _suffix_subtitle(min_gas_mgas, title_suffix)
                else ""
            )
        ),
    )
    _save(fig, output_path)
    return output_path


def plot_gas_vs_latency_scatter(
    df: pd.DataFrame, output_path: Path, min_gas_mgas: float = 0.0, title_suffix: str = ""
) -> Path:
    """Scatter plot of gas used vs latency with trend line."""
    filtered = _filter_or_raise(df, min_gas_mgas)

    fig, ax = _subplots(figsize=(8.0, 4.5))
    ax.scatter(
        filtered["gas_used_mgas"],
        filtered["elapsed_ms"],
        color=_EXEC_COLOR,
        s=30,
        alpha=0.55,
        edgecolors="none",
        zorder=10,
    )

    slope: float | None = None
    if len(filtered) > 1:
        z = np.polyfit(filtered["gas_used_mgas"], filtered["elapsed_ms"], 1)
        p = np.poly1d(z)
        slope = float(z[0])
        x_line = np.linspace(filtered["gas_used_mgas"].min(), filtered["gas_used_mgas"].max(), 100)
        ax.plot(
            x_line,
            p(x_line),
            color=_TREND_COLOR,
            linestyle="--",
            linewidth=1.6,
            zorder=11,
        )

    ax.set_xlabel("Gas used (Mgas)")
    ax.set_ylabel("Total latency (ms)")
    _subtitle_title(
        ax,
        (
            f"Latency rises {slope:.2f} ms per Mgas"
            if slope is not None
            else "Gas used vs total latency"
        ),
        subtitle=_suffix_subtitle(min_gas_mgas, title_suffix) or None,
    )
    _save(fig, output_path)
    return output_path


def generate_all_graphs(
    blocks: list[BlockMetrics],
    output_dir: Path,
    min_gas_mgas: float = 0.0,
    title_suffix: str = "",
) -> list[Path]:
    """Generate all performance graphs and return paths to created files."""
    output_dir.mkdir(parents=True, exist_ok=True)
    df = metrics_to_dataframe(blocks)

    suffix = f"_min{int(min_gas_mgas)}mgas" if min_gas_mgas > 0 else ""
    paths: list[Path] = []
    paths.append(
        plot_gas_throughput(
            df,
            output_dir / f"gas_throughput{suffix}.png",
            min_gas_mgas=min_gas_mgas,
            title_suffix=title_suffix,
        )
    )
    paths.append(
        plot_latency_breakdown(
            df,
            output_dir / f"latency_breakdown{suffix}.png",
            min_gas_mgas=min_gas_mgas,
            title_suffix=title_suffix,
        )
    )
    paths.append(
        plot_latency_percentage(
            df,
            output_dir / f"latency_percentage{suffix}.png",
            min_gas_mgas=min_gas_mgas,
            title_suffix=title_suffix,
        )
    )
    paths.append(
        plot_gas_vs_latency_scatter(
            df,
            output_dir / f"gas_vs_latency{suffix}.png",
            min_gas_mgas=min_gas_mgas,
            title_suffix=title_suffix,
        )
    )
    return paths
