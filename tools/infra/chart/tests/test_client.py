import base64
import unittest

import matplotlib.pyplot as plt
import pandas as pd
from tools.infra.chart.client import ChartClient, _apply_annotations, _prepare_time_axis


class TimeAxisTests(unittest.TestCase):
    def test_iso_dates_are_parsed_and_sorted(self):
        frame = pd.DataFrame(
            [
                {"date": "2026-03-10", "value": 2},
                {"date": "2026-01-01", "value": 1},
            ]
        )

        prepared, is_time_axis = _prepare_time_axis(frame, "date")

        self.assertTrue(is_time_axis)
        self.assertEqual(
            prepared["date"].dt.strftime("%Y-%m-%d").tolist(),
            ["2026-01-01", "2026-03-10"],
        )

    def test_category_labels_are_not_coerced(self):
        frame = pd.DataFrame([{"label": "Series A", "value": 1}])

        prepared, is_time_axis = _prepare_time_axis(frame, "label")

        self.assertFalse(is_time_axis)
        self.assertEqual(prepared["label"].tolist(), ["Series A"])

    def test_regions_and_events_render_on_time_axis(self):
        fig, ax = plt.subplots()
        ax.plot(pd.to_datetime(["2026-01-01", "2026-04-01"]), [0, 1])

        _apply_annotations(
            ax,
            [
                {"start": "2026-02-01", "end": "2026-02-10", "label": "Trade window"},
                {"date": "2026-03-01", "label": "Single trade"},
            ],
            True,
        )

        self.assertEqual(len(ax.patches), 1)
        self.assertEqual(len(ax.lines), 2)
        self.assertEqual(
            [text.get_text() for text in ax.texts],
            ["Trade window", "Single trade"],
        )
        plt.close(fig)

    def test_render_chart_accepts_time_regions(self):
        encoded = ChartClient().render_chart(
            chart_type="line",
            data=[
                {"date": "2026-01-01", "PF return": 0},
                {"date": "2026-04-01", "PF return": 12},
            ],
            title="PF gained over the period",
            x="date",
            y="PF return",
            extras={
                "annotations": [{"start": "2026-02-01", "end": "2026-02-14", "label": "Purchases"}]
            },
        )

        self.assertTrue(base64.b64decode(encoded).startswith(b"\x89PNG"))


if __name__ == "__main__":
    unittest.main()
