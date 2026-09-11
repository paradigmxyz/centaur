"""Chart tool: render common charts to base64 PNGs for Slack upload."""

from __future__ import annotations

import base64
from io import BytesIO
from typing import Any

import matplotlib
import pandas as pd

matplotlib.use("Agg")

import matplotlib.dates as mdates
import matplotlib.pyplot as plt

_OKABE_ITO = ["#0072B2", "#D55E00", "#009E73", "#CC79A7", "#F0E442", "#56B4E9", "#E69F00"]


def _prepare_datetime_x(df: pd.DataFrame, x_col: str, extras: dict[str, Any]) -> bool:
    """Parse and sort an explicitly requested chronological x-axis."""
    if not extras.get("datetime_x"):
        return False

    parsed = pd.to_datetime(df[x_col], errors="raise")
    if parsed.dt.tz is not None:
        parsed = parsed.dt.tz_convert(None)
    df[x_col] = parsed
    df.sort_values(x_col, inplace=True)
    df.reset_index(drop=True, inplace=True)
    return True


def _apply_comparison_band(
    ax: plt.Axes,
    df: pd.DataFrame,
    x_col: str,
    extras: dict[str, Any],
) -> None:
    band = extras.get("comparison_band")
    if not isinstance(band, dict):
        return

    lower = band.get("lower")
    upper = band.get("upper")
    if lower not in df.columns or upper not in df.columns:
        return

    ax.fill_between(
        df[x_col],
        pd.to_numeric(df[lower], errors="coerce"),
        pd.to_numeric(df[upper], errors="coerce"),
        color=band.get("color", "#0072B2"),
        alpha=float(band.get("alpha", 0.12)),
        linewidth=0,
        label=band.get("label"),
        zorder=1,
    )


def _apply_event_regions(ax: plt.Axes, extras: dict[str, Any], datetime_x: bool) -> None:
    regions = extras.get("event_regions")
    if not isinstance(regions, list):
        return

    for index, region in enumerate(regions):
        if not isinstance(region, dict) or "start" not in region:
            continue
        start: Any = region["start"]
        end: Any = region.get("end", start)
        if datetime_x:
            start = pd.Timestamp(start)
            end = pd.Timestamp(end)
            if end <= start:
                end = start + pd.Timedelta(days=1)
        color = region.get("color", _OKABE_ITO[(index + 2) % len(_OKABE_ITO)])
        alpha = float(region.get("alpha", 0.10))
        ax.axvspan(start, end, color=color, alpha=alpha, linewidth=0, zorder=0)

        label = region.get("label")
        if label:
            midpoint = start + (end - start) / 2
            ax.text(
                midpoint,
                float(region.get("label_y", 0.97)),
                str(label),
                transform=ax.get_xaxis_transform(),
                ha="center",
                va="top",
                fontsize=float(region.get("fontsize", 7.5)),
                color=region.get("text_color", "#374151"),
                rotation=float(region.get("rotation", 0)),
                zorder=4,
            )


def _pick_x(df: pd.DataFrame, hint: str | None) -> str:
    if hint and hint in df.columns:
        return hint
    return str(df.columns[0])


def _numeric_columns(df: pd.DataFrame, x_col: str) -> list[str]:
    return [str(col) for col in df.select_dtypes(include="number").columns if col != x_col]


def _pick_y(df: pd.DataFrame, x_col: str, hint: str | list[str] | None) -> list[str]:
    if isinstance(hint, str) and hint in df.columns:
        return [hint]
    if isinstance(hint, list):
        cols = [col for col in hint if col in df.columns]
        if cols:
            return cols
    numeric = _numeric_columns(df, x_col)
    if numeric:
        return numeric[:4]
    return [str(df.columns[1])] if len(df.columns) > 1 else [x_col]


def _style_axes(ax: plt.Axes, title: str, subtitle: str | None, source: str) -> None:
    ax.set_title(title or "Chart", loc="left", fontsize=15, fontweight=700, pad=16)
    if subtitle:
        ax.text(0, 1.02, subtitle, transform=ax.transAxes, ha="left", va="bottom", fontsize=10)
    if source:
        ax.text(
            0,
            -0.18,
            source,
            transform=ax.transAxes,
            ha="left",
            va="top",
            fontsize=8,
            color="#666666",
        )
    ax.spines["top"].set_visible(False)
    ax.spines["right"].set_visible(False)
    ax.grid(axis="y", color="#E5E7EB", linewidth=0.8)


def _figure_to_base64(fig: plt.Figure) -> str:
    buf = BytesIO()
    fig.tight_layout()
    fig.savefig(buf, format="png", dpi=200, bbox_inches="tight")
    plt.close(fig)
    return base64.b64encode(buf.getvalue()).decode("utf-8")


class ChartClient:
    """Chart builder. Public API is intentionally one method: render_chart."""

    def render_chart(
        self,
        chart_type: str,
        data: list[dict[str, Any]],
        title: str = "",
        question: str = "",
        protagonist: str | None = None,
        subtitle: str | None = None,
        source: str = "",
        theme_mode: str = "light",
        x: str | None = None,
        y: str | list[str] | None = None,
        extras: dict[str, Any] | None = None,
    ) -> str:
        """Render a chart and return base64-encoded PNG bytes.

        Args:
            chart_type: Free-form type: line, bar, top, indexed_line, scatter,
                candlestick, drawdown, heatmap, sparkline, etc. Aliases are
                normalized by the router.
            data: Row-oriented records suitable for ``pandas.DataFrame``.
            title: Sentence-case takeaway title.
            question: Optional source question / intent.
            protagonist: Optional series/category to highlight.
            subtitle: Optional units/baseline/range subtitle.
            source: Optional source line.
            theme_mode: light | dark | editorial.
            x/y: Optional column hints; otherwise first/numeric columns are used.
            extras: Optional handler-specific settings. Line charts support
                ``datetime_x``, ``comparison_band``, ``event_regions``,
                ``x_label``, ``y_label``, and ``legend_loc``.
        """
        if not data:
            return ""

        del question, protagonist, theme_mode
        extras = extras or {}

        df = pd.DataFrame(data)
        if df.empty:
            return ""

        chart_kind = chart_type.lower().replace("_", "-")
        x_col = _pick_x(df, x)
        y_cols = _pick_y(df, x_col, y)
        datetime_x = _prepare_datetime_x(df, x_col, extras)

        fig, ax = plt.subplots(figsize=(8, 4.5))
        if chart_kind in {"pie", "pie-chart", "donut", "donut-chart"}:
            value_col = y_cols[0]
            ax.pie(
                df[value_col],
                labels=df[x_col].astype(str),
                autopct="%1.1f%%",
                startangle=90,
                colors=_OKABE_ITO,
                wedgeprops={"linewidth": 1, "edgecolor": "white"},
            )
            ax.set_title(title or "Chart", loc="left", fontsize=15, fontweight=700, pad=16)
            if subtitle:
                ax.text(
                    0,
                    1.02,
                    subtitle,
                    transform=ax.transAxes,
                    ha="left",
                    va="bottom",
                    fontsize=10,
                )
            if source:
                ax.text(
                    0,
                    -0.08,
                    source,
                    transform=ax.transAxes,
                    ha="left",
                    va="top",
                    fontsize=8,
                    color="#666666",
                )
            ax.axis("equal")
            return _figure_to_base64(fig)

        if chart_kind in {"bar", "bar-chart", "top"}:
            width = 0.8 / max(1, len(y_cols))
            positions = range(len(df))
            for idx, col in enumerate(y_cols):
                offsets = [pos + (idx - (len(y_cols) - 1) / 2) * width for pos in positions]
                ax.bar(
                    offsets,
                    df[col],
                    width=width,
                    label=col,
                    color=_OKABE_ITO[idx % len(_OKABE_ITO)],
                )
            ax.set_xticks(list(positions))
            ax.set_xticklabels(
                df[x_col].astype(str),
                rotation=extras.get("x_rotation", 30),
                ha="right",
            )
        elif chart_kind in {"scatter", "scatter-plot"}:
            value_col = y_cols[0]
            ax.scatter(df[x_col], df[value_col], color=_OKABE_ITO[0], alpha=0.75, edgecolors="none")
        else:
            _apply_event_regions(ax, extras, datetime_x)
            _apply_comparison_band(ax, df, x_col, extras)
            for idx, col in enumerate(y_cols):
                ax.plot(
                    df[x_col],
                    df[col],
                    marker="o",
                    linewidth=1.8,
                    label=col,
                    color=_OKABE_ITO[idx % len(_OKABE_ITO)],
                    zorder=3,
                )
            if datetime_x:
                locator = mdates.AutoDateLocator(minticks=4, maxticks=10)
                ax.xaxis.set_major_locator(locator)
                ax.xaxis.set_major_formatter(mdates.ConciseDateFormatter(locator))
            elif len(df) > 6:
                ax.tick_params(axis="x", labelrotation=30)

        ax.set_xlabel(extras.get("x_label", x_col))
        ax.set_ylabel(extras.get("y_label", ", ".join(y_cols)))
        if len(y_cols) > 1:
            ax.legend(frameon=False, loc=extras.get("legend_loc", "best"))
        _style_axes(ax, title, subtitle, source)
        return _figure_to_base64(fig)


def _client() -> ChartClient:
    return ChartClient()
