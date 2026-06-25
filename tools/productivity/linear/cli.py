"""CLI for Linear issue tracking."""

import base64
import json
import sys

import typer
from rich.console import Console

from centaur_sdk import Table

app = typer.Typer(name="linear", help="Linear CLI for AI agents")


@app.command("health")
def health():
    """Assert linear connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.me()
        payload = {"ok": True, "tool": "linear", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "linear", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    """Get Linear client with env loading."""
    from pathlib import Path

    from dotenv import load_dotenv

    # Try local .env first, then repo root
    cli_env = Path(__file__).parent.parent.parent / ".env"
    repo_env = Path(__file__).parent.parent.parent.parent.parent / ".env"

    for env_file in [cli_env, repo_env]:
        if env_file.exists():
            load_dotenv(env_file)
            break

    from .client import LinearClient

    return LinearClient()


def require_mutation_success(result: dict, action: str) -> None:
    """Exit with an error if a mutation result carries success=False."""
    if not result.get("success"):
        console.print(f"[red]Linear reported the {action} failed.[/]")
        raise typer.Exit(1)


@app.command()
def me():
    """Show authenticated user info."""
    client = get_client()
    user = client.me()
    console.print(f"[bold]{user.get('name')}[/] ({user.get('email')})")


@app.command()
def teams():
    """List all teams."""
    client = get_client()
    result = client.teams()

    if not result:
        console.print("[yellow]No teams found.[/]")
        raise typer.Exit()

    table = Table(title=f"Teams ({len(result)})")
    table.add_column("Key", style="cyan", max_width=10)
    table.add_column("Name", style="white", max_width=30)
    table.add_column("Description", style="dim", max_width=50)

    for team in result:
        table.add_row(
            team.get("key", ""), team.get("name", ""), (team.get("description") or "")[:50]
        )

    console.print(table)


@app.command()
def issues(
    team: str = typer.Option(None, "--team", "-t", help="Filter by team key (e.g., ENG)"),
    assignee: str = typer.Option(
        None, "--assignee", "-a", help="Filter by assignee (use 'me' for self)"
    ),
    state: str = typer.Option(None, "--state", "-s", help="Filter by state name"),
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    full: bool = typer.Option(False, "--full", "-f", help="Show full details"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List issues with filters.

    Examples:
        linear issues --team ENG --assignee me
        linear issues --state "In Progress" -n 50
        linear issues -t ENG -s Done --json
    """
    client = get_client()
    result = client.issues(team_key=team, assignee=assignee, state=state, limit=limit)

    if not result:
        console.print("[yellow]No issues found.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if full:
        for issue in result:
            state_name = issue.get("state", {}).get("name", "")
            assignee_name = (
                issue.get("assignee", {}).get("name", "") if issue.get("assignee") else ""
            )
            console.print(f"\n[bold cyan]{issue.get('identifier')}[/] {issue.get('title')}")
            console.print(f"  State: [green]{state_name}[/]  Assignee: {assignee_name or '-'}")
            if issue.get("description"):
                desc = issue["description"][:200].replace("\n", " ")
                console.print(f"  {desc}{'...' if len(issue['description']) > 200 else ''}")
            console.print(f"  [dim]{issue.get('url')}[/]")
    else:
        table = Table(title=f"Issues ({len(result)})")
        table.add_column("ID", style="cyan", max_width=12)
        table.add_column("Title", style="white", max_width=50)
        table.add_column("State", style="green", max_width=15)
        table.add_column("Assignee", style="yellow", max_width=15)

        for issue in result:
            state_name = issue.get("state", {}).get("name", "")
            assignee_name = (
                issue.get("assignee", {}).get("name", "") if issue.get("assignee") else ""
            )
            title = issue.get("title", "")[:50]
            table.add_row(issue.get("identifier", ""), title, state_name, assignee_name)

        console.print(table)


@app.command()
def issue(
    issue_id: str = typer.Argument(..., help="Issue ID or identifier (e.g., ENG-123)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get details of a specific issue."""
    client = get_client()
    result = client.issue(issue_id)

    if not result:
        console.print(f"[red]Issue '{issue_id}' not found.[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    console.print(f"\n[bold cyan]{result.get('identifier')}[/] {result.get('title')}")
    console.print(f"Team: {result.get('team', {}).get('name', '')}")
    console.print(f"State: [green]{result.get('state', {}).get('name', '')}[/]")
    console.print(f"Priority: {result.get('priorityLabel', '')}")

    if result.get("assignee"):
        console.print(f"Assignee: {result['assignee'].get('name', '')}")
    if result.get("project"):
        console.print(f"Project: {result['project'].get('name', '')}")
    if result.get("cycle"):
        console.print(f"Cycle: {result['cycle'].get('name', '')}")

    labels = result.get("labels", {}).get("nodes", [])
    if labels:
        label_names = ", ".join(lbl.get("name", "") for lbl in labels)
        console.print(f"Labels: {label_names}")

    if result.get("description"):
        console.print(f"\n[bold]Description:[/]\n{result['description']}")

    comments = result.get("comments", {}).get("nodes", [])
    if comments:
        console.print(f"\n[bold]Comments ({len(comments)}):[/]")
        for c in comments[:5]:
            console.print(
                f"  [dim]{c.get('user', {}).get('name', '')}:[/] {c.get('body', '')[:100]}"
            )

    children = result.get("children", {}).get("nodes", [])
    if children:
        console.print(f"\n[bold]Sub-issues ({len(children)}):[/]")
        for child in children:
            console.print(
                f"  {child.get('identifier')} - {child.get('title')} [{child.get('state', {}).get('name', '')}]"
            )

    console.print(f"\n[dim]{result.get('url')}[/]")


@app.command("fetch-asset")
def fetch_asset(
    url: str = typer.Argument(..., help="A https://uploads.linear.app/... asset URL"),
    output: str = typer.Option(
        None, "--output", "-o", help="Write the bytes here instead of printing metadata"
    ),
):
    """Download a Linear-hosted asset (e.g. an embedded screenshot)."""
    client = get_client()
    result = client.fetch_asset(url)

    if output:
        from pathlib import Path

        Path(output).write_bytes(base64.b64decode(result["data"]))
        console.print(
            f"[green]Wrote {result['byte_length']} bytes[/] to {output} ({result['mime_type']})"
        )
        raise typer.Exit()

    meta = {k: v for k, v in result.items() if k != "data"}
    console.print(json.dumps(meta, indent=2))


@app.command()
def create(
    title: str = typer.Argument(..., help="Issue title"),
    team: str = typer.Option(..., "--team", "-t", help="Team key (e.g., ENG)"),
    description: str = typer.Option(None, "--description", "-d", help="Issue description"),
    assignee: str = typer.Option(None, "--assignee", "-a", help="Assignee name"),
    due_date: str = typer.Option(None, "--due-date", help="Due date as YYYY-MM-DD"),
    priority: int = typer.Option(
        None, "--priority", "-p", help="Priority (0=none, 1=urgent, 2=high, 3=medium, 4=low)"
    ),
    parent: str = typer.Option(
        None, "--parent", help="Parent issue identifier (e.g., ENG-123) for sub-issues"
    ),
):
    """Create a new issue.

    Examples:
        linear create "Fix login bug" --team ENG
        linear create "New feature" -t ENG -d "Description here" -p 2
        linear create "Sub-task" -t ENG --parent ENG-123
    """
    client = get_client()

    teams_list = client.teams()
    team_match = next((t for t in teams_list if t.get("key", "").upper() == team.upper()), None)
    if not team_match:
        console.print(f"[red]Team '{team}' not found.[/]")
        raise typer.Exit(1)

    assignee_id = None
    if assignee:
        if assignee.lower() == "me":
            me_info = client.me()
            assignee_id = me_info.get("id")
        else:
            users = client.users()
            user_match = next(
                (u for u in users if assignee.lower() in u.get("name", "").lower()),
                None,
            )
            if user_match:
                assignee_id = user_match.get("id")

    parent_id = None
    if parent:
        parent_issue = client.issue(parent)
        if not parent_issue:
            console.print(f"[red]Parent issue '{parent}' not found.[/]")
            raise typer.Exit(1)
        parent_id = parent_issue.get("id")

    result = client.create_issue(
        title=title,
        team_id=team_match["id"],
        description=description,
        assignee_id=assignee_id,
        due_date=due_date,
        priority=priority,
        parent_id=parent_id,
    )

    require_mutation_success(result, "issue creation")

    console.print(f"[green]Created:[/] [bold]{result.get('identifier')}[/] {result.get('title')}")
    console.print(f"[dim]{result.get('url')}[/]")


@app.command()
def update(
    issue_id: str = typer.Argument(..., help="Issue ID or identifier (e.g., ENG-123)"),
    title: str = typer.Option(None, "--title", help="New title"),
    state: str = typer.Option(None, "--state", "-s", help="New state name"),
    assignee: str = typer.Option(None, "--assignee", "-a", help="Assignee name (or 'me')"),
    due_date: str = typer.Option(None, "--due-date", help="Due date as YYYY-MM-DD"),
    priority: int = typer.Option(None, "--priority", "-p", help="Priority (0-4)"),
    project: str = typer.Option(None, "--project", help="Project name to add issue to"),
):
    """Update an existing issue.

    Examples:
        linear update ENG-123 --state "In Progress"
        linear update ENG-123 --assignee me
        linear update ENG-123 --project "Q1 Roadmap"
    """
    client = get_client()

    current = client.issue(issue_id)
    if not current:
        console.print(f"[red]Issue '{issue_id}' not found.[/]")
        raise typer.Exit(1)

    state_id = None
    if state:
        team_key = current.get("team", {}).get("key")
        states = client.workflow_states(team_key)
        state_match = next(
            (s for s in states if state.lower() in s.get("name", "").lower()),
            None,
        )
        if state_match:
            state_id = state_match.get("id")
        else:
            console.print(f"[red]State '{state}' not found.[/]")
            raise typer.Exit(1)

    assignee_id = None
    if assignee:
        if assignee.lower() == "me":
            me_info = client.me()
            assignee_id = me_info.get("id")
        else:
            users = client.users()
            user_match = next(
                (u for u in users if assignee.lower() in u.get("name", "").lower()),
                None,
            )
            if user_match:
                assignee_id = user_match.get("id")

    project_id = None
    if project:
        projects_list = client.projects()
        project_match = next(
            (p for p in projects_list if project.lower() in p.get("name", "").lower()),
            None,
        )
        if project_match:
            project_id = project_match.get("id")
        else:
            console.print(f"[red]Project '{project}' not found.[/]")
            raise typer.Exit(1)

    result = client.update_issue(
        issue_id=issue_id,
        title=title,
        state_id=state_id,
        assignee_id=assignee_id,
        due_date=due_date,
        priority=priority,
        project_id=project_id,
    )

    require_mutation_success(result, "issue update")

    console.print(f"[green]Updated:[/] [bold]{result.get('identifier')}[/] {result.get('title')}")
    console.print(f"State: {result.get('state', {}).get('name', '')}")
    if result.get("project"):
        console.print(f"Project: {result.get('project', {}).get('name', '')}")
    console.print(f"[dim]{result.get('url')}[/]")


@app.command()
def comment(
    issue_id: str = typer.Argument(..., help="Issue ID or identifier"),
    body: str = typer.Argument(..., help="Comment text (markdown supported)"),
):
    """Add a comment to an issue.

    Examples:
        linear comment ENG-123 "Fixed in commit abc123"
    """
    client = get_client()
    result = client.add_comment(issue_id, body)

    require_mutation_success(result, "comment creation")
    console.print(f"[green]Comment added to {issue_id}[/]")


@app.command()
def projects(
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List all projects."""
    client = get_client()
    result = client.projects(limit=limit)

    if not result:
        console.print("[yellow]No projects found.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    table = Table(title=f"Projects ({len(result)})")
    table.add_column("Name", style="cyan", max_width=30)
    table.add_column("State", style="green", max_width=12)
    table.add_column("Progress", style="yellow", max_width=10)
    table.add_column("Lead", style="white", max_width=15)
    table.add_column("Target", style="dim", max_width=12)

    for proj in result:
        progress = f"{int(proj.get('progress', 0) * 100)}%"
        lead = proj.get("lead", {}).get("name", "") if proj.get("lead") else ""
        table.add_row(
            proj.get("name", "")[:30],
            proj.get("state", ""),
            progress,
            lead,
            proj.get("targetDate") or "",
        )

    console.print(table)


@app.command("project")
def project_detail(
    project_name: str = typer.Argument(..., help="Project name (partial match supported)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get details of a specific project.

    Examples:
        linear project "Q1 Roadmap"
        linear project roadmap --json
    """
    client = get_client()
    projects_list = client.projects()

    project_match = next(
        (p for p in projects_list if project_name.lower() in p.get("name", "").lower()),
        None,
    )
    if not project_match:
        console.print(f"[red]Project '{project_name}' not found.[/]")
        raise typer.Exit(1)

    result = client.project(project_match["id"])
    if not result:
        console.print("[red]Failed to fetch project details.[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    console.print(f"\n[bold cyan]{result.get('name')}[/]")
    console.print(f"State: [green]{result.get('state', '')}[/]")
    console.print(f"Progress: {int(result.get('progress', 0) * 100)}%")

    if result.get("lead"):
        console.print(f"Lead: {result['lead'].get('name', '')}")
    if result.get("startDate"):
        console.print(f"Start: {result.get('startDate')}")
    if result.get("targetDate"):
        console.print(f"Target: {result.get('targetDate')}")
    if result.get("description"):
        console.print(f"\n[bold]Description:[/]\n{result['description']}")

    teams = result.get("teams", {}).get("nodes", [])
    if teams:
        team_names = ", ".join(t.get("key", "") for t in teams)
        console.print(f"Teams: {team_names}")

    issues = result.get("issues", {}).get("nodes", [])
    if issues:
        console.print(f"\n[bold]Issues ({len(issues)}):[/]")
        for iss in issues[:10]:
            console.print(
                f"  {iss.get('identifier')} - {iss.get('title')[:50]} [{iss.get('state', {}).get('name', '')}]"
            )
        if len(issues) > 10:
            console.print(f"  ... and {len(issues) - 10} more")

    console.print(f"\n[dim]{result.get('url')}[/]")


@app.command()
def cycles(
    team: str = typer.Option(None, "--team", "-t", help="Filter by team key"),
    limit: int = typer.Option(10, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List cycles."""
    client = get_client()
    result = client.cycles(team_key=team, limit=limit)

    if not result:
        console.print("[yellow]No cycles found.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    table = Table(title=f"Cycles ({len(result)})")
    table.add_column("Team", style="cyan", max_width=10)
    table.add_column("Cycle", style="white", max_width=20)
    table.add_column("Progress", style="green", max_width=10)
    table.add_column("Dates", style="dim", max_width=25)

    for cycle in result:
        team_key = cycle.get("team", {}).get("key", "")
        name = cycle.get("name") or f"Cycle {cycle.get('number', '')}"
        progress = f"{int(cycle.get('progress', 0) * 100)}%"
        dates = f"{cycle.get('startsAt', '')[:10]} → {cycle.get('endsAt', '')[:10]}"
        table.add_row(team_key, name, progress, dates)

    console.print(table)


@app.command()
def states(
    team: str = typer.Option(None, "--team", "-t", help="Filter by team key"),
):
    """List workflow states."""
    client = get_client()
    result = client.workflow_states(team_key=team)

    if not result:
        console.print("[yellow]No states found.[/]")
        raise typer.Exit()

    table = Table(title=f"Workflow States ({len(result)})")
    table.add_column("Team", style="cyan", max_width=10)
    table.add_column("State", style="white", max_width=20)
    table.add_column("Type", style="green", max_width=15)

    for state in sorted(
        result, key=lambda s: (s.get("team", {}).get("key", ""), s.get("position", 0))
    ):
        table.add_row(
            state.get("team", {}).get("key", ""),
            state.get("name", ""),
            state.get("type", ""),
        )

    console.print(table)


@app.command("labels")
def list_labels(
    team: str = typer.Option(None, "--team", "-t", help="Filter by team key"),
):
    """List issue labels."""
    client = get_client()
    result = client.labels(team_key=team)

    if not result:
        console.print("[yellow]No labels found.[/]")
        raise typer.Exit()

    table = Table(title=f"Labels ({len(result)})")
    table.add_column("Team", style="cyan", max_width=10)
    table.add_column("Label", style="white", max_width=25)

    for label in sorted(
        result, key=lambda lbl: ((lbl.get("team") or {}).get("key", ""), lbl.get("name", ""))
    ):
        team = label.get("team")
        team_key = team.get("key", "") if team else "org"
        table.add_row(team_key, label.get("name", ""))

    console.print(table)


@app.command("add-label")
def add_label(
    issue_id: str = typer.Argument(..., help="Issue ID or identifier (e.g., ENG-123)"),
    label: str = typer.Argument(..., help="Label name to add"),
    team: str = typer.Option(None, "--team", "-t", help="Team key, to bind a team-scoped label"),
):
    """Add a single label to an issue (leaves other labels untouched)."""
    client = get_client()
    result = client.add_label(issue_id, label, team_key=team)
    ok = result.get("success")
    console.print(
        f"[green]Added[/] '{label}' to {issue_id}."
        if ok
        else f"[red]Failed[/] to add '{label}' to {issue_id}."
    )
    if not ok:
        raise typer.Exit(1)


@app.command("remove-label")
def remove_label(
    issue_id: str = typer.Argument(..., help="Issue ID or identifier (e.g., ENG-123)"),
    label: str = typer.Argument(..., help="Label name to remove"),
    team: str = typer.Option(None, "--team", "-t", help="Team key, to bind a team-scoped label"),
):
    """Remove a single label from an issue (leaves other labels untouched)."""
    client = get_client()
    result = client.remove_label(issue_id, label, team_key=team)
    ok = result.get("success")
    console.print(
        f"[green]Removed[/] '{label}' from {issue_id}."
        if ok
        else f"[red]Failed[/] to remove '{label}' from {issue_id}."
    )
    if not ok:
        raise typer.Exit(1)


@app.command()
def users_cmd(
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    query: str = typer.Option(None, "--query", "-q", help="Filter by name"),
):
    """List workspace users."""
    client = get_client()
    result = client.users(limit=limit)

    if query:
        result = [u for u in result if query.lower() in u.get("name", "").lower()]

    if not result:
        console.print("[yellow]No users found.[/]")
        raise typer.Exit()

    table = Table(title=f"Users ({len(result)})")
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Email", style="white", max_width=35)
    table.add_column("Active", style="green", max_width=8)

    for user in result:
        active = "✓" if user.get("active") else ""
        table.add_row(user.get("name", ""), user.get("email", ""), active)

    console.print(table)


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query"),
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search issues by text.

    Examples:
        linear search "login bug"
        linear search "authentication" --json
    """
    client = get_client()
    result = client.search_issues(query, limit=limit)

    if not result:
        console.print("[yellow]No issues found.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    table = Table(title=f"Search: '{query}' ({len(result)} results)")
    table.add_column("ID", style="cyan", max_width=12)
    table.add_column("Title", style="white", max_width=50)
    table.add_column("State", style="green", max_width=15)
    table.add_column("Team", style="yellow", max_width=8)

    for issue in result:
        table.add_row(
            issue.get("identifier", ""),
            issue.get("title", "")[:50],
            issue.get("state", {}).get("name", ""),
            issue.get("team", {}).get("key", ""),
        )

    console.print(table)


@app.command("weekly")
def weekly_report_cmd(
    team: str = typer.Option(None, "--team", "-t", help="Filter by team key"),
    limit: int = typer.Option(30, "--limit", "-n", help="Max issues"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    github_org: str = typer.Option("", "--org", help="GitHub org for link search"),
):
    """Weekly report: issues from last 7 days with Slack & GitHub links.

    Examples:
        linear weekly
        linear weekly --team CHAIN
        linear weekly --json
    """
    from .integrations import weekly_report

    console.print("[dim]Fetching issues and searching for related links...[/]")
    result = weekly_report(team_key=team, github_org=github_org, limit=limit)

    if not result:
        console.print("[yellow]No issues from last week.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    console.print(f"\n[bold]Weekly Report ({len(result)} issues)[/]\n")

    for issue in result:
        state_name = issue.get("state", {}).get("name", "")
        team_key = issue.get("team", {}).get("key", "")
        console.print(
            f"[bold cyan]{issue.get('identifier')}[/] [{team_key}] {issue.get('title')[:60]}"
        )
        console.print(f"  State: [green]{state_name}[/]  {issue.get('url', '')}")

        # Slack link
        slack = issue.get("slack_link")
        if slack:
            console.print(
                f"  [yellow]Slack:[/] #{slack.get('channel', '')} - {slack.get('permalink', '')}"
            )
        else:
            console.print("  [dim]Slack: -[/]")

        # GitHub link
        gh = issue.get("github_link")
        if gh:
            if gh.get("type") == "pr":
                console.print(
                    f"  [magenta]GitHub PR:[/] {gh.get('title', '')[:50]} - {gh.get('url', '')}"
                )
            else:
                console.print(
                    f"  [magenta]GitHub:[/] {gh.get('message', '')[:50]} - {gh.get('url', '')}"
                )
        else:
            console.print("  [dim]GitHub: -[/]")

        console.print()


@app.command("link")
def link_issues(
    issue_id: str = typer.Argument(..., help="Issue ID (e.g., ENG-123)"),
    related_id: str = typer.Argument(..., help="Related issue ID (e.g., ENG-456)"),
    relation: str = typer.Option(
        "blocks",
        "--type",
        "-t",
        help="Relation type: blocks, blocked-by, related, duplicate",
    ),
):
    """Link two issues with a dependency or relation.

    Examples:
        linear link ENG-123 ENG-456                    # ENG-123 blocks ENG-456
        linear link ENG-123 ENG-456 -t blocked-by      # ENG-123 is blocked by ENG-456
        linear link ENG-123 ENG-456 -t related         # Mark as related
        linear link ENG-123 ENG-456 -t duplicate       # Mark as duplicate
    """
    client = get_client()

    # Map user-friendly names to API types
    relation_map = {
        "blocks": ("blocks", False),
        "blocked-by": ("blocks", True),  # Swap the order
        "related": ("related", False),
        "duplicate": ("duplicate", False),
    }

    if relation.lower() not in relation_map:
        console.print(
            f"[red]Invalid relation type '{relation}'. Use: blocks, blocked-by, related, duplicate[/]"
        )
        raise typer.Exit(1)

    api_type, swap = relation_map[relation.lower()]

    # Swap issue order for blocked-by (so the API creates: related_id blocks issue_id)
    if swap:
        issue_id, related_id = related_id, issue_id

    result = client.create_issue_relation(issue_id, related_id, api_type)

    if result.get("success"):
        rel = result.get("issueRelation", {})
        issue = rel.get("issue", {})
        related = rel.get("relatedIssue", {})
        rel_type = rel.get("type", "")

        if rel_type == "blocks":
            console.print(
                f"[green]Linked:[/] [bold]{issue.get('identifier')}[/] blocks [bold]{related.get('identifier')}[/]"
            )
        else:
            console.print(
                f"[green]Linked:[/] [bold]{issue.get('identifier')}[/] ↔ [bold]{related.get('identifier')}[/] ({rel_type})"
            )
    else:
        console.print("[red]Failed to create relation.[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
