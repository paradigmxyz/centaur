"""CLI for Vercel — passes everything through to the official `vercel` binary."""

from __future__ import annotations

import os

import typer
from dotenv import load_dotenv

load_dotenv()

app = typer.Typer(name="vercel", add_completion=False)


@app.command(
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
)
def main(ctx: typer.Context) -> None:
    """Official Vercel CLI with credentials handled for you.

    All arguments pass through unchanged to the npm `vercel` binary with
    `--token` appended (iron-proxy injects the real credential on requests to
    api.vercel.com). Examples:

        vercel whoami

        vercel ls

        vercel inspect <deployment-url> --logs

    For the underlying CLI's own help, run `vercel help` or
    `vercel help <command>`.
    """
    from .client import vercel_binary, with_token

    args = list(ctx.args)
    if not args:
        typer.echo(ctx.get_help())
        raise typer.Exit(0)

    binary = vercel_binary()
    os.execv(binary, [binary, *with_token(args)])
