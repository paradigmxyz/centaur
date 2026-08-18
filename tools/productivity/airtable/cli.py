"""CLI for Airtable."""

import json

import typer
from dotenv import load_dotenv
from rich.console import Console

from .client import AirtableClient

load_dotenv()

app = typer.Typer(name="airtable", help="Airtable API client")


@app.command("health")
def health():
    """Assert airtable connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.health()
        payload = {"ok": True, "tool": "airtable", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "airtable", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def _print(data: object) -> None:
    console.print_json(json.dumps(data, default=str))


def _json_object(value: str) -> dict:
    parsed = json.loads(value)
    if not isinstance(parsed, dict):
        raise typer.BadParameter("Expected a JSON object.")
    return parsed


def _json_object_list(value: str) -> list[dict]:
    parsed = json.loads(value)
    if not isinstance(parsed, list) or not all(isinstance(item, dict) for item in parsed):
        raise typer.BadParameter("Expected a JSON array of objects.")
    return parsed


def _comma_list(value: str) -> list[str]:
    return [item.strip() for item in value.split(",") if item.strip()]


@app.command()
def whoami() -> None:
    """Show the current Airtable API key identity."""
    _print(AirtableClient().whoami())


@app.command()
def bases(limit: int = typer.Option(100, "--limit", "-n")) -> None:
    """List visible Airtable bases."""
    _print(AirtableClient().list_bases(limit=limit))


@app.command()
def schema(base_id: str) -> None:
    """Get a base schema."""
    _print(AirtableClient().schema(base_id))


@app.command()
def records(
    base_id: str,
    table: str,
    view: str | None = typer.Option(None, "--view"),
    max_records: int = typer.Option(100, "--max-records", "-n"),
) -> None:
    """List records from a table or view."""
    _print(AirtableClient().list_records(base_id, table, view=view, max_records=max_records))


@app.command()
def from_url(url: str, max_records: int = typer.Option(50, "--max-records", "-n")) -> None:
    """Read a compact snapshot from an Airtable table/view URL."""
    _print(AirtableClient().snapshot_from_url(url, max_records=max_records))


@app.command()
def create_record(
    base_id: str,
    table: str,
    fields: str = typer.Option(..., "--fields", help="Record fields as a JSON object."),
    typecast: bool = typer.Option(False, "--typecast", help="Let Airtable coerce field values."),
) -> None:
    """Create one record."""
    _print(AirtableClient().create_record(base_id, table, _json_object(fields), typecast=typecast))


@app.command()
def create_records(
    base_id: str,
    table: str,
    records: str = typer.Option(
        ...,
        "--records",
        help="Records as a JSON array. Items may be field objects or objects with a fields property.",
    ),
    typecast: bool = typer.Option(False, "--typecast", help="Let Airtable coerce field values."),
) -> None:
    """Create records."""
    _print(
        AirtableClient().create_records(
            base_id, table, _json_object_list(records), typecast=typecast
        )
    )


@app.command()
def update_record(
    base_id: str,
    table: str,
    record_id: str,
    fields: str = typer.Option(..., "--fields", help="Updated fields as a JSON object."),
    typecast: bool = typer.Option(False, "--typecast", help="Let Airtable coerce field values."),
    replace: bool = typer.Option(False, "--replace", help="Replace all writable fields with PUT."),
) -> None:
    """Update one record."""
    _print(
        AirtableClient().update_record(
            base_id,
            table,
            record_id,
            _json_object(fields),
            typecast=typecast,
            replace=replace,
        )
    )


@app.command()
def update_records(
    base_id: str,
    table: str,
    records: str = typer.Option(
        ...,
        "--records",
        help="Updates as a JSON array of objects with id and fields properties.",
    ),
    typecast: bool = typer.Option(False, "--typecast", help="Let Airtable coerce field values."),
    replace: bool = typer.Option(False, "--replace", help="Replace all writable fields with PUT."),
) -> None:
    """Update records."""
    _print(
        AirtableClient().update_records(
            base_id,
            table,
            _json_object_list(records),
            typecast=typecast,
            replace=replace,
        )
    )


@app.command()
def upsert_records(
    base_id: str,
    table: str,
    records: str = typer.Option(
        ...,
        "--records",
        help="Records as a JSON array. Items may be field objects or objects with a fields property.",
    ),
    merge_fields: str = typer.Option(
        ...,
        "--merge-fields",
        help="Comma-separated field names for Airtable performUpsert matching.",
    ),
    typecast: bool = typer.Option(False, "--typecast", help="Let Airtable coerce field values."),
) -> None:
    """Create or update records by merge fields."""
    _print(
        AirtableClient().upsert_records(
            base_id,
            table,
            _json_object_list(records),
            fields_to_merge_on=_comma_list(merge_fields),
            typecast=typecast,
        )
    )


@app.command()
def delete_record(base_id: str, table: str, record_id: str) -> None:
    """Delete one record."""
    _print(AirtableClient().delete_record(base_id, table, record_id))


@app.command()
def delete_records(
    base_id: str,
    table: str,
    record_ids: str = typer.Option(
        ..., "--record-ids", help="Comma-separated Airtable record IDs."
    ),
) -> None:
    """Delete records."""
    _print(AirtableClient().delete_records(base_id, table, _comma_list(record_ids)))


if __name__ == "__main__":
    app()
