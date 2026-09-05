from __future__ import annotations

import base64

import matplotlib.pyplot as plt
import pandas as pd
from client import ChartClient, _apply_comparison_band, _apply_event_regions, _prepare_datetime_x


def test_prepare_datetime_x_sorts_chronologically() -> None:
    frame = pd.DataFrame({"date": ["2025-02-01", "2025-01-01"], "value": [2, 1]})

    assert _prepare_datetime_x(frame, "date", {"datetime_x": True})
    assert frame["date"].tolist() == [pd.Timestamp("2025-01-01"), pd.Timestamp("2025-02-01")]


def test_band_and_event_region_are_drawn() -> None:
    frame = pd.DataFrame(
        {
            "date": pd.to_datetime(["2025-01-01", "2025-02-01"]),
            "actual": [100, 110],
            "counterfactual": [100, 103],
        }
    )
    _, ax = plt.subplots()

    _apply_comparison_band(
        ax,
        frame,
        "date",
        {"comparison_band": {"lower": "counterfactual", "upper": "actual"}},
    )
    _apply_event_regions(
        ax,
        {"event_regions": [{"start": "2025-01-10", "end": "2025-01-20", "label": "Trade"}]},
        True,
    )

    assert len(ax.collections) == 1
    assert len(ax.patches) == 1
    assert [text.get_text() for text in ax.texts] == ["Trade"]
    plt.close(ax.figure)


def test_render_datetime_chart_with_band_and_region_returns_png() -> None:
    encoded = ChartClient().render_chart(
        chart_type="line",
        data=[
            {"date": "2025-01-01", "actual": 100, "counterfactual": 100},
            {"date": "2025-03-01", "actual": 115, "counterfactual": 105},
        ],
        x="date",
        y=["actual", "counterfactual"],
        extras={
            "datetime_x": True,
            "comparison_band": {"lower": "counterfactual", "upper": "actual"},
            "event_regions": [{"start": "2025-01-15", "end": "2025-02-01", "label": "Buy"}],
        },
    )

    assert base64.b64decode(encoded).startswith(b"\x89PNG\r\n\x1a\n")
