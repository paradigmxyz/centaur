"""CLI for Pylon support platform."""

import json
import sys

from dotenv import load_dotenv

load_dotenv()

import typer
from rich.console import Console
from centaur_sdk import Table

app = typer.Typer(name="pylon", help="Pylon CLI for AI agents")


@app.command("health")
def health():
    """Assert pylon connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_me()
        payload = {"ok": True, "tool": "pylon", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "pylon", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def _get_client():
    from .client import PylonClient

    return PylonClient()


@app.command()
def me():
    """Get organization details for the current API token."""
    client = _get_client()

    try:
        result = client.get_me()
        data = result.get("data", {})
        console.print(f"[bold]Organization:[/] {data.get('name', 'N/A')}")
        console.print(f"[dim]ID: {data.get('id', 'N/A')}[/]")
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def issues(
    days: int = typer.Option(7, "--days", "-d", help="Days to look back"),
    state: str = typer.Option(None, "--state", "-s", help="Filter by state"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    full: bool = typer.Option(False, "--full", "-f", help="Show full details"),
):
    """List recent issues.

    Examples:
        pylon issues
        pylon issues --days 30 --state open
        pylon issues -n 10 -f
    """
    client = _get_client()

    try:
        results = client.list_issues(days=days)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if state:
        results = [i for i in results if i.get("state", "").lower() == state.lower()]

    results = results[:limit]

    if not results:
        console.print("[yellow]No issues found.[/]")
        raise typer.Exit()

    if full:
        for issue in results:
            console.print(f"\n[bold cyan]#{issue.get('number', 'N/A')}[/] {issue.get('title', '')}")
            console.print(
                f"[dim]State: {issue.get('state')} | Priority: {issue.get('priority', 'N/A')}[/]"
            )
            console.print(f"[dim]ID: {issue.get('id')}[/]")
            if issue.get("requester"):
                console.print(f"[green]Requester:[/] {issue['requester'].get('email', 'N/A')}")
            if issue.get("assignee"):
                console.print(f"[green]Assignee:[/] {issue['assignee'].get('name', 'N/A')}")
            console.print("---")
    else:
        table = Table(title=f"Issues (last {days} days)")
        table.add_column("#", style="cyan", max_width=8)
        table.add_column("Title", style="white", max_width=50)
        table.add_column("State", style="green", max_width=15)
        table.add_column("Priority", style="yellow", max_width=10)
        table.add_column("Requester", style="dim", max_width=25)

        for issue in results:
            requester = issue.get("requester", {}).get("email", "") or ""
            table.add_row(
                str(issue.get("number", "")),
                (issue.get("title", "")[:50] + "...")
                if len(issue.get("title", "")) > 50
                else issue.get("title", ""),
                issue.get("state", ""),
                issue.get("priority", ""),
                requester[:25],
            )

        console.print(table)


@app.command()
def issue(
    issue_id: str = typer.Argument(..., help="Issue ID or number"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get details of a specific issue.

    Examples:
        pylon issue 123
        pylon issue abc-123-def --json
    """
    client = _get_client()

    try:
        result = client.get_issue(issue_id)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    console.print(f"\n[bold cyan]#{result.get('number', 'N/A')}[/] {result.get('title', '')}")
    console.print(f"[dim]ID: {result.get('id')}[/]")
    console.print(f"\n[bold]State:[/] {result.get('state', 'N/A')}")
    console.print(f"[bold]Priority:[/] {result.get('priority', 'N/A')}")

    if result.get("requester"):
        req = result["requester"]
        console.print(f"[bold]Requester:[/] {req.get('name', '')} <{req.get('email', '')}>")

    if result.get("assignee"):
        console.print(f"[bold]Assignee:[/] {result['assignee'].get('name', 'N/A')}")

    if result.get("team"):
        console.print(f"[bold]Team:[/] {result['team'].get('name', 'N/A')}")

    if result.get("account"):
        console.print(f"[bold]Account:[/] {result['account'].get('name', 'N/A')}")

    if result.get("tags"):
        console.print(f"[bold]Tags:[/] {', '.join(result['tags'])}")

    console.print(f"\n[bold]Created:[/] {result.get('created_at', 'N/A')}")
    console.print(f"[bold]Updated:[/] {result.get('updated_at', 'N/A')}")


@app.command()
def issue_create(
    title: str = typer.Argument(..., help="Issue title"),
    body: str = typer.Argument(..., help="Issue body (HTML supported)"),
    requester: str = typer.Option(None, "--requester", "-r", help="Requester email"),
    account: str = typer.Option(None, "--account", "-a", help="Account ID"),
    assignee: str = typer.Option(None, "--assignee", help="Assignee user ID"),
    priority: str = typer.Option(None, "--priority", "-p", help="urgent/high/medium/low"),
    tags: str = typer.Option(None, "--tags", "-t", help="Comma-separated tags"),
    team: str = typer.Option(None, "--team", help="Team ID"),
):
    """Create a new issue.

    Examples:
        pylon issue-create "Bug report" "Something is broken"
        pylon issue-create "Help needed" "Please help" --requester user@example.com
        pylon issue-create "Urgent" "Fix now" -p urgent -t bug,critical
    """
    client = _get_client()

    tag_list = [t.strip() for t in tags.split(",")] if tags else None

    try:
        result = client.create_issue(
            title=title,
            body_html=body,
            requester_email=requester,
            account_id=account,
            assignee_id=assignee,
            priority=priority,
            tags=tag_list,
            team_id=team,
        )
        console.print(f"[green]✓ Created issue #{result.get('number', 'N/A')}[/]")
        console.print(f"[dim]ID: {result.get('id')}[/]")
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def issue_update(
    issue_id: str = typer.Argument(..., help="Issue ID or number"),
    state: str = typer.Option(
        None, "--state", "-s", help="new/waiting_on_you/waiting_on_customer/on_hold/closed"
    ),
    assignee: str = typer.Option(None, "--assignee", help="Assignee user ID (empty to unassign)"),
    tags: str = typer.Option(None, "--tags", "-t", help="Comma-separated tags (replaces existing)"),
    team: str = typer.Option(None, "--team", help="Team ID (empty to unassign)"),
):
    """Update an existing issue.

    Examples:
        pylon issue-update 123 --state closed
        pylon issue-update 123 --assignee user-id-here
        pylon issue-update 123 -t bug,resolved
    """
    client = _get_client()

    tag_list = [t.strip() for t in tags.split(",")] if tags else None

    try:
        result = client.update_issue(
            issue_id=issue_id,
            state=state,
            assignee_id=assignee,
            tags=tag_list,
            team_id=team,
        )
        console.print(f"[green]✓ Updated issue #{result.get('number', 'N/A')}[/]")
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def issue_search(
    query: str = typer.Argument(..., help="Search field:operator:value (e.g., state:equals:open)"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search issues with filters.

    Filter format: field:operator:value

    Examples:
        pylon issue-search "state:equals:open"
        pylon issue-search "title:string_contains:bug"
        pylon issue-search "assignee_id:is_unset:" --json
    """
    client = _get_client()

    parts = query.split(":", 2)
    if len(parts) < 2:
        console.print("[red]Invalid filter format. Use field:operator:value[/]")
        raise typer.Exit(1)

    field = parts[0]
    operator = parts[1]
    value = parts[2] if len(parts) > 2 else None

    filter_obj = {"field": field, "operator": operator}
    if value:
        filter_obj["value"] = value

    try:
        result = client.search_issues(filter_obj, limit=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    issues_data = result.get("data", [])

    if json_output:
        print(json.dumps(issues_data, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    if not issues_data:
        console.print("[yellow]No issues found.[/]")
        raise typer.Exit()

    table = Table(title=f"Search Results ({len(issues_data)})")
    table.add_column("#", style="cyan", max_width=8)
    table.add_column("Title", style="white", max_width=50)
    table.add_column("State", style="green", max_width=15)
    table.add_column("Priority", style="yellow", max_width=10)

    for issue in issues_data:
        table.add_row(
            str(issue.get("number", "")),
            (issue.get("title", "")[:50] + "...")
            if len(issue.get("title", "")) > 50
            else issue.get("title", ""),
            issue.get("state", ""),
            issue.get("priority", ""),
        )

    console.print(table)


@app.command()
def accounts(
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    query: str = typer.Option(None, "--query", "-q", help="Filter by name"),
):
    """List accounts.

    Examples:
        pylon accounts
        pylon accounts -q "acme"
    """
    client = _get_client()

    try:
        result = client.list_accounts(limit=limit)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    accounts_data = result.get("data", [])

    if query:
        accounts_data = [a for a in accounts_data if query.lower() in a.get("name", "").lower()]

    if not accounts_data:
        console.print("[yellow]No accounts found.[/]")
        raise typer.Exit()

    table = Table(title=f"Accounts ({len(accounts_data)})")
    table.add_column("Name", style="cyan", max_width=30)
    table.add_column("Domain", style="white", max_width=30)
    table.add_column("Tags", style="dim", max_width=30)

    for account in accounts_data:
        domains = account.get("domains") or []
        domain_str = domains[0] if domains else ""
        tags = ", ".join((account.get("tags") or [])[:3])
        table.add_row(
            account.get("name", "")[:30],
            domain_str[:30],
            tags[:30],
        )

    console.print(table)


@app.command()
def account(
    account_id: str = typer.Argument(..., help="Account ID or external ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get details of a specific account.

    Examples:
        pylon account abc-123
        pylon account abc-123 --json
    """
    client = _get_client()

    try:
        result = client.get_account(account_id)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    console.print(f"\n[bold cyan]{result.get('name', 'N/A')}[/]")
    console.print(f"[dim]ID: {result.get('id')}[/]")

    if result.get("domains"):
        console.print(f"[bold]Domains:[/] {', '.join(result['domains'])}")

    if result.get("owner"):
        console.print(f"[bold]Owner:[/] {result['owner'].get('name', 'N/A')}")

    if result.get("tags"):
        console.print(f"[bold]Tags:[/] {', '.join(result['tags'])}")

    console.print(f"\n[bold]Created:[/] {result.get('created_at', 'N/A')}")


@app.command()
def account_create(
    name: str = typer.Argument(..., help="Account name"),
    domain: str = typer.Option(None, "--domain", "-d", help="Primary domain"),
    tags: str = typer.Option(None, "--tags", "-t", help="Comma-separated tags"),
    owner: str = typer.Option(None, "--owner", "-o", help="Owner user ID"),
):
    """Create a new account.

    Examples:
        pylon account-create "Acme Corp"
        pylon account-create "Acme Corp" -d acme.com -t enterprise,priority
    """
    client = _get_client()

    domains = [domain] if domain else None
    tag_list = [t.strip() for t in tags.split(",")] if tags else None

    try:
        result = client.create_account(
            name=name,
            domains=domains,
            primary_domain=domain,
            tags=tag_list,
            owner_id=owner,
        )
        console.print(f"[green]✓ Created account: {result.get('name')}[/]")
        console.print(f"[dim]ID: {result.get('id')}[/]")
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@app.command()
def contacts(
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    query: str = typer.Option(None, "--query", "-q", help="Filter by name/email"),
):
    """List contacts.

    Examples:
        pylon contacts
        pylon contacts -q "john"
    """
    client = _get_client()

    try:
        results = client.list_contacts()
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if query:
        query_lower = query.lower()
        results = [
            c
            for c in results
            if query_lower in c.get("name", "").lower() or query_lower in c.get("email", "").lower()
        ]

    results = results[:limit]

    if not results:
        console.print("[yellow]No contacts found.[/]")
        raise typer.Exit()

    table = Table(title=f"Contacts ({len(results)})")
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Email", style="white", max_width=35)
    table.add_column("Account", style="dim", max_width=25)

    for contact in results:
        account_name = contact.get("account", {}).get("name", "") if contact.get("account") else ""
        table.add_row(
            contact.get("name", "")[:25],
            contact.get("email", "")[:35],
            account_name[:25],
        )

    console.print(table)


@app.command()
def contact(
    contact_id: str = typer.Argument(..., help="Contact ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get details of a specific contact."""
    client = _get_client()

    try:
        result = client.get_contact(contact_id)
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    console.print(f"\n[bold cyan]{result.get('name', 'N/A')}[/]")
    console.print(f"[dim]ID: {result.get('id')}[/]")
    console.print(f"[bold]Email:[/] {result.get('email', 'N/A')}")

    if result.get("account"):
        console.print(f"[bold]Account:[/] {result['account'].get('name', 'N/A')}")


@app.command()
def users(
    query: str = typer.Option(None, "--query", "-q", help="Filter by name/email"),
):
    """List users in the organization."""
    client = _get_client()

    try:
        results = client.list_users()
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if query:
        query_lower = query.lower()
        results = [
            u
            for u in results
            if query_lower in u.get("name", "").lower() or query_lower in u.get("email", "").lower()
        ]

    if not results:
        console.print("[yellow]No users found.[/]")
        raise typer.Exit()

    table = Table(title=f"Users ({len(results)})")
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Email", style="white", max_width=35)
    table.add_column("Role", style="dim", max_width=15)

    for user in results:
        table.add_row(
            user.get("name", "")[:25],
            user.get("email", "")[:35],
            user.get("role", {}).get("name", "") if user.get("role") else "",
        )

    console.print(table)


@app.command()
def teams():
    """List teams in the organization."""
    client = _get_client()

    try:
        results = client.list_teams()
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if not results:
        console.print("[yellow]No teams found.[/]")
        raise typer.Exit()

    table = Table(title=f"Teams ({len(results)})")
    table.add_column("Name", style="cyan", max_width=30)
    table.add_column("ID", style="dim", max_width=40)

    for team in results:
        table.add_row(team.get("name", ""), team.get("id", ""))

    console.print(table)


@app.command()
def tags(
    object_type: str = typer.Option(
        None, "--type", "-t", help="Filter by type: account/issue/contact"
    ),
):
    """List all tags."""
    client = _get_client()

    try:
        results = client.list_tags()
    except RuntimeError as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)

    if object_type:
        results = [t for t in results if t.get("object_type", "").lower() == object_type.lower()]

    if not results:
        console.print("[yellow]No tags found.[/]")
        raise typer.Exit()

    table = Table(title=f"Tags ({len(results)})")
    table.add_column("Value", style="cyan", max_width=30)
    table.add_column("Type", style="white", max_width=15)
    table.add_column("Color", style="dim", max_width=10)

    for tag in results:
        table.add_row(
            tag.get("value", ""),
            tag.get("object_type", ""),
            tag.get("hex_color", ""),
        )

    console.print(table)


if __name__ == "__main__":
    app()
