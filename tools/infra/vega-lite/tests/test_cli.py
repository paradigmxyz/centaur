import re

from centaur_tool_vega_lite.cli import app
from typer.testing import CliRunner

runner = CliRunner()
ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


def test_main_help_links_the_specification_reference():
    result = runner.invoke(app, ["--help"])

    help_text = " ".join(ANSI_ESCAPE.sub("", result.stdout).split())
    assert result.exit_code == 0
    assert "https://vega.github.io/vega-lite/docs/" in help_text
    assert "Keep data inline" in help_text
    assert "vega-lite render spec.json --output chart.png" in help_text


def test_render_help_explains_file_contract():
    result = runner.invoke(app, ["render", "--help"])

    assert result.exit_code == 0
    assert "external data and image requests are" in result.stdout
    assert "blocked" in result.stdout
    assert "JSON metadata" in result.stdout
