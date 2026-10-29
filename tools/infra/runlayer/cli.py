"""Discover and call Runlayer tools as the person requesting the Centaur turn."""
import json
import typer
from .client import RunlayerClient, RunlayerError

app = typer.Typer(no_args_is_help=True, help=__doc__)


def _run(operation):
    try:
        with RunlayerClient() as client:
            typer.echo(json.dumps(operation(client), indent=2))
    except (RunlayerError, ValueError) as error:
        data = error.as_dict() if isinstance(error, RunlayerError) else {"error": str(error)}
        typer.echo(json.dumps(data), err=True)
        raise typer.Exit(1) from None


@app.command()
def discover():
    """List the current person's exact tool names and input schemas."""
    _run(lambda client: {"tools": client.list_tools()})


@app.command()
def search(query: str):
    """Find available tools by capability."""
    _run(lambda client: {"results": client.search(query)})


@app.command()
def call(name: str, arguments: str):
    """Call an exact advertised tool name with a JSON argument object."""
    try:
        parsed = json.loads(arguments)
    except ValueError:
        typer.echo('{"error":"Arguments must be valid JSON."}', err=True)
        raise typer.Exit(2) from None
    _run(lambda client: {"result": client.call_tool(name, parsed)})
