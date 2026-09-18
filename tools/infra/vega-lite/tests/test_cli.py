from centaur_tool_vega_lite.cli import app
from typer.testing import CliRunner

runner = CliRunner()


def test_main_help_explains_agent_workflow():
    result = runner.invoke(app, ["--help"])

    assert result.exit_code == 0
    assert "Agent workflow" in result.stdout
    assert "data.values" in result.stdout
    assert "vega-lite render spec.json --output chart.png" in result.stdout
    assert "platform upload tool" in result.stdout


def test_render_help_explains_file_contract():
    result = runner.invoke(app, ["render", "--help"])

    assert result.exit_code == 0
    assert "external data and image requests are" in result.stdout
    assert "blocked" in result.stdout
    assert "JSON metadata" in result.stdout
