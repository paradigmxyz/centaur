import base64
import struct
import unittest

from centaur_tool_vega_lite.client import VegaLiteClient

BAR_SPEC = {
    "data": {"values": [{"label": "A", "value": 3}, {"label": "B", "value": 5}]},
    "mark": "bar",
    "encoding": {
        "x": {"field": "label", "type": "nominal"},
        "y": {"field": "value", "type": "quantitative"},
    },
}


class VegaLiteClientTests(unittest.TestCase):
    def test_renders_inline_spec_to_png_with_useful_default_size(self):
        rendered = base64.b64decode(VegaLiteClient().render(BAR_SPEC, scale=1))

        self.assertTrue(rendered.startswith(b"\x89PNG"))
        width, height = struct.unpack(">II", rendered[16:24])
        self.assertEqual((width, height), (800, 450))

    def test_renders_inline_spec_to_svg_with_default_style(self):
        encoded = VegaLiteClient().render(BAR_SPEC, output_format="svg")

        rendered = base64.b64decode(encoded).decode("utf-8")
        self.assertIn("<svg", rendered[:500])
        self.assertIn("#F8F9FA", rendered)
        self.assertIn("#0072B2", rendered)
        self.assertIn("Inter", rendered)

    def test_rejects_external_data_url(self):
        spec = {
            "data": {"url": "https://example.com/data.csv"},
            "mark": "bar",
            "encoding": {"x": {"field": "category", "type": "nominal"}},
        }

        with self.assertRaisesRegex(ValueError, "External data URLs are not allowed"):
            VegaLiteClient().render(spec)

    def test_rejects_oversized_dimensions(self):
        spec = {**BAR_SPEC, "width": 2000}

        with self.assertRaisesRegex(ValueError, "width must be between"):
            VegaLiteClient().render(spec)

    def test_does_not_treat_data_field_as_chart_dimension(self):
        spec = {
            "data": {"values": [{"width": 2000, "count": 1}]},
            "mark": "bar",
            "encoding": {
                "x": {"field": "width", "type": "quantitative"},
                "y": {"field": "count", "type": "quantitative"},
            },
        }

        rendered = base64.b64decode(VegaLiteClient().render(spec, scale=1))

        self.assertTrue(rendered.startswith(b"\x89PNG"))

    def test_rejects_too_many_named_dataset_rows(self):
        spec = {
            "datasets": {"points": [{}] * 50_001},
            "data": {"name": "points"},
            "mark": "point",
        }

        with self.assertRaisesRegex(ValueError, "50001 inline values"):
            VegaLiteClient().render(spec)

    def test_compiler_rejects_invalid_spec(self):
        with self.assertRaises(ValueError):
            VegaLiteClient().render({"mark": {"type": "not-a-mark"}})


if __name__ == "__main__":
    unittest.main()
