"""CLI for Preqin API."""

from dotenv import load_dotenv

load_dotenv()

import json
from typing import Any

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(name="preqin", help="Preqin Operational API and Feeds API CLI")


@app.command("health")
def health():
    """Assert preqin connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.auth_health()
        if isinstance(details, dict) and not details.get("ok", False):
            raise RuntimeError(str(details.get("error") or "preqin health check failed"))
        payload = {"ok": True, "tool": "preqin", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "preqin", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import PreqinClient

    return PreqinClient()


def _records_from_response(data: Any) -> list[dict[str, Any]]:
    if isinstance(data, list):
        return [item for item in data if isinstance(item, dict)]
    if not isinstance(data, dict):
        return []
    for key in ("data", "items", "results", "funds", "fundManagers", "fundManager"):
        value = data.get(key)
        if isinstance(value, list):
            return [item for item in value if isinstance(item, dict)]
    for value in data.values():
        if isinstance(value, list):
            dicts = [item for item in value if isinstance(item, dict)]
            if dicts:
                return dicts
    return []


def _first(record: dict[str, Any], *keys: str) -> Any:
    lowered = {key.casefold(): value for key, value in record.items()}
    for key in keys:
        if key in record and record[key] not in (None, ""):
            return record[key]
        value = lowered.get(key.casefold())
        if value not in (None, ""):
            return value
    return None


def _print_records(data: dict[str, Any] | list[dict[str, Any]], title: str) -> None:
    records = _records_from_response(data)
    if not records:
        console.print(f"[yellow]No records found in {title} response.[/]")
        return
    table = Table(title=title)
    table.add_column("ID")
    table.add_column("Name")
    table.add_column("Manager")
    table.add_column("Status")
    table.add_column("Size")
    table.add_column("Strategy")
    for record in records[:25]:
        table.add_row(
            str(
                _first(record, "FundId", "FundID", "fundId", "FundManagerID", "FundManagerId", "id")
                or ""
            ),
            str(_first(record, "FundName", "name", "fundName", "FundManagerName") or ""),
            str(_first(record, "FundManagerName", "manager", "firmName") or ""),
            str(_first(record, "Status", "FundStatus", "status") or ""),
            str(_first(record, "FundSize", "Size", "fundSize", "FinalCloseSize") or ""),
            str(_first(record, "Strategy", "FundStrategy", "strategy") or ""),
        )
    console.print(table)


def _exit_error(exc: Exception, json_output: bool) -> None:
    if json_output:
        print(json.dumps({"ok": False, "error": str(exc)}, indent=2))
    else:
        console.print(f"[red]Error:[/] {exc}")
    raise typer.Exit(1)


@app.command("credential-status")
def credential_status(json_output: bool = typer.Option(False, "--json", help="Output as JSON")):
    """Show which Preqin secret names resolve, without printing secret values."""
    data = get_client().credential_status()
    if json_output:
        print(json.dumps(data, indent=2))
        return
    table = Table(title="Preqin credential status")
    table.add_column("Secret")
    table.add_column("Present")
    table.add_column("Length")
    for name, info in data.items():
        table.add_row(name, str(info["present"]), str(info["length"]))
    console.print(table)


@app.command("auth-health")
def auth_health(json_output: bool = typer.Option(False, "--json", help="Output as JSON")):
    """Check Preqin Operational API auth."""
    data = get_client().auth_health()
    if json_output:
        print(json.dumps(data, indent=2))
        return
    if data.get("ok"):
        console.print("[green]Preqin auth OK[/]")
    else:
        console.print(f"[red]Preqin auth failed:[/] {data.get('error')}")


@app.command("fund-managers")
def fund_managers(
    name: str | None = typer.Option(None, "--name", help="Fund manager name search"),
    fund_manager_id: str | None = typer.Option(None, "--id", help="Fund manager ID"),
    asset_class: str | None = typer.Option(
        None, "--asset-class", help="Asset class, e.g. pe,re,hf"
    ),
    include: str | None = typer.Option(
        None, "--include", help="Asset-class-specific data to include"
    ),
    size: int = typer.Option(20, "--size", help="Records per page, max 200"),
    page: int = typer.Option(1, "--page", help="Page number"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search Preqin fund managers."""
    try:
        data = get_client().get_fund_managers(
            fund_manager_name=name,
            fund_manager_id=fund_manager_id,
            asset_class=asset_class,
            include=include,
            size=size,
            page=page,
        )
    except RuntimeError as exc:
        _exit_error(exc, json_output)
    if json_output:
        print(json.dumps(data, indent=2))
        return
    _print_records(data, "Fund managers")


@app.command("funds")
def funds(
    name: str | None = typer.Option(None, "--name", help="Fund name search"),
    fund_id: str | None = typer.Option(None, "--id", help="Fund ID"),
    manager: str | None = typer.Option(None, "--manager", help="Fund manager name search"),
    manager_id: str | None = typer.Option(None, "--manager-id", help="Fund manager ID"),
    asset_class: str | None = typer.Option(
        None, "--asset-class", help="Asset class, e.g. pe,re,hf"
    ),
    strategy: str | None = typer.Option(None, "--strategy", help="Fund strategy"),
    status: str | None = typer.Option(None, "--status", help="Fund status"),
    include: str | None = typer.Option(
        None, "--include", help="Asset-class-specific data to include"
    ),
    size: int = typer.Option(20, "--size", help="Records per page, max 200"),
    page: int = typer.Option(1, "--page", help="Page number"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search Preqin funds."""
    try:
        data = get_client().get_funds(
            fund_name=name,
            fund_id=fund_id,
            fund_manager_name=manager,
            fund_manager_id=manager_id,
            asset_class=asset_class,
            strategy=strategy,
            status=status,
            include=include,
            size=size,
            page=page,
        )
    except RuntimeError as exc:
        _exit_error(exc, json_output)
    if json_output:
        print(json.dumps(data, indent=2))
        return
    _print_records(data, "Funds")


@app.command("find-paradigm-xyz")
def find_paradigm_xyz(
    size: int = typer.Option(20, "--size", help="Records per query"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Find Paradigm XYZ fund-manager and fund records."""
    try:
        data = get_client().find_paradigm_xyz(size=size)
    except RuntimeError as exc:
        _exit_error(exc, json_output)
    if json_output:
        print(json.dumps(data, indent=2))
        return
    _print_records(data.get("fund_managers", {}), "Paradigm XYZ fund-manager matches")
    _print_records(data.get("funds_by_manager", {}), "Paradigm XYZ funds by manager")
    _print_records(data.get("funds_by_name_fallback", {}), "Paradigm funds by name fallback")


@app.command("feed-specs")
def feed_specs(json_output: bool = typer.Option(False, "--json", help="Output as JSON")):
    """List public Preqin Feeds API OpenAPI spec versions."""
    data = get_client().list_feed_specs()
    if json_output:
        print(json.dumps(data, indent=2))
        return
    table = Table(title="Preqin Feeds API specs")
    table.add_column("Version")
    table.add_column("URL")
    for item in data:
        table.add_row(str(item.get("version", "")), str(item.get("url", "")))
    console.print(table)


if __name__ == "__main__":
    app()
