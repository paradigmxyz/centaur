"""CLI for Ashby ATS."""

from dotenv import load_dotenv

load_dotenv()

import json
import subprocess
import sys
from datetime import datetime, timezone

import typer
from centaur_sdk import Table
from rich.console import Console

app = typer.Typer(name="ashby", help="Ashby ATS CLI for AI agents")


@app.command("health")
def health():
    """Assert ashby connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.api_key_info()
        payload = {"ok": True, "tool": "ashby", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "ashby", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_pmadmin_employees() -> tuple[set[str], set[str]]:
    """Fetch all employees (current and former) from pmadmin database.

    Returns:
        Tuple of (employee_emails, normalized_employee_names)
    """
    employee_emails: set[str] = set()
    employee_names: set[str] = set()

    try:
        result = subprocess.run(
            [
                "reshift",
                "db",
                "-n",
                "500",
                'SELECT u.email, p."fullName" FROM "User" u '
                'JOIN "Person" p ON u."personId" = p.id '
                "WHERE u.email LIKE '%@paradigm.xyz';",
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        if result.returncode == 0:
            for line in result.stdout.strip().split("\n"):
                if "@paradigm.xyz" in line and "│" in line:
                    parts = [p.strip() for p in line.split("│")]
                    if len(parts) >= 3:
                        email = parts[1].strip().lower()
                        name = parts[2].strip()
                        if email and email.endswith("@paradigm.xyz"):
                            employee_emails.add(email)
                        if name:
                            employee_names.add(normalize_name(name))
    except (subprocess.TimeoutExpired, FileNotFoundError, Exception):
        pass

    return employee_emails, employee_names


def get_client():
    """Get Ashby client."""
    from .client import AshbyClient

    return AshbyClient()


def format_date(date_str: str | None) -> str:
    """Format ISO date to readable format."""
    if not date_str:
        return "-"
    try:
        dt = datetime.fromisoformat(date_str.replace("Z", "+00:00"))
        return dt.strftime("%Y-%m-%d %H:%M")
    except (ValueError, AttributeError):
        return str(date_str)


def truncate_id(id_str: str | None, length: int = 12) -> str:
    """Truncate ID for display."""
    if not id_str:
        return "-"
    if len(id_str) > length:
        return id_str[:length] + "..."
    return id_str


# ============== Jobs ==============


@app.command()
def jobs(
    status: str = typer.Option(None, "--status", "-s", help="Filter: open/closed/draft/archived"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List all jobs."""
    client = get_client()
    result = client.jobs(status=status, limit=limit)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print("[yellow]No jobs found.[/]")
        raise typer.Exit()

    table = Table(title=f"Jobs ({len(result)})")
    table.add_column("ID", style="cyan", max_width=15)
    table.add_column("Title", style="white", max_width=40)
    table.add_column("Status", style="green", max_width=10)
    table.add_column("Department", max_width=20)
    table.add_column("Location", max_width=20)

    for job in result:
        dept = job.get("department", {})
        loc = job.get("location", {})
        table.add_row(
            truncate_id(job.get("id")),
            job.get("title", "-"),
            job.get("status", "-"),
            dept.get("name", "-") if dept else "-",
            loc.get("name", "-") if loc else "-",
        )

    console.print(table)


@app.command()
def job(
    job_id: str = typer.Argument(..., help="Job ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get job details."""
    client = get_client()
    result = client.job(job_id)

    if not result:
        console.print(f"[red]Job '{job_id}' not found.[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    console.print(f"\n[bold cyan]Job: {result.get('title', 'Unknown')}[/]")
    console.print(f"ID: {result.get('id', '-')}")
    console.print(f"Status: [green]{result.get('status', '-')}[/]")

    dept = result.get("department", {})
    if dept:
        console.print(f"Department: {dept.get('name', '-')}")

    loc = result.get("location", {})
    if loc:
        console.print(f"Location: {loc.get('name', '-')}")

    console.print(f"Created: {format_date(result.get('createdAt'))}")


# ============== Candidates ==============


@app.command()
def candidates(
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List candidates."""
    client = get_client()
    result = client.candidates(limit=limit)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print("[yellow]No candidates found.[/]")
        raise typer.Exit()

    table = Table(title=f"Candidates ({len(result)})")
    table.add_column("ID", style="cyan", max_width=15)
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Email", max_width=30)
    table.add_column("Source", max_width=20)
    table.add_column("Created", max_width=18)

    for c in result:
        emails = c.get("emailAddresses", [])
        email = emails[0].get("value", "-") if emails else "-"
        source = c.get("source", {})
        table.add_row(
            truncate_id(c.get("id")),
            c.get("name", "-"),
            email,
            source.get("title", "-") if source else "-",
            format_date(c.get("createdAt")),
        )

    console.print(table)


@app.command()
def candidate(
    candidate_id: str = typer.Argument(..., help="Candidate ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get candidate details including background info (position, company, school)."""
    client = get_client()
    result = client.candidate(candidate_id)

    if not result:
        console.print(f"[red]Candidate '{candidate_id}' not found.[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    console.print(f"\n[bold cyan]Candidate: {result.get('name', 'Unknown')}[/]")
    console.print(f"ID: {result.get('id', '-')}")

    for e in result.get("emailAddresses", []):
        console.print(f"Email: {e.get('value', '-')} ({e.get('type', 'primary')})")

    for p in result.get("phoneNumbers", []):
        console.print(f"Phone: {p.get('value', '-')} ({p.get('type', 'primary')})")

    position = result.get("position")
    company = result.get("company")
    school = result.get("school")

    if position or company:
        console.print("\n[bold]Background:[/]")
        if position:
            console.print(f"  Position: {position}")
        if company:
            console.print(f"  Company: {company}")
        if school:
            console.print(f"  School: {school}")
    elif school:
        console.print("\n[bold]Background:[/]")
        console.print(f"  School: {school}")

    social_links = result.get("socialLinks", [])
    if social_links:
        console.print("\n[bold]Links:[/]")
        for link in social_links:
            link_type = link.get("type", "Link")
            url = link.get("url", "-")
            console.print(f"  {link_type}: {url}")

    file_handles = result.get("fileHandles", [])
    has_resume = any(fh.get("type") == "Resume" for fh in file_handles)
    if has_resume:
        console.print("\n[bold]Resume:[/] Available (use --json to get file handle)")

    source = result.get("source", {})
    if source:
        console.print(f"Source: {source.get('title', '-')}")

    console.print(f"Created: {format_date(result.get('createdAt'))}")

    apps = result.get("applications", [])
    if apps:
        console.print(f"\n[bold]Applications ({len(apps)}):[/]")
        for a in apps:
            job = a.get("job", {})
            stage = a.get("currentInterviewStage", {})
            console.print(f"  - {job.get('title', 'Unknown')} | Stage: {stage.get('name', '-')}")


@app.command("candidate-search")
def candidate_search(
    query: str = typer.Argument(..., help="Search query (name or email)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    by_email: bool = typer.Option(False, "--email", "-e", help="Search by email instead of name"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search candidates by name or email.

    By default searches by name. Use --email to search by email.
    If query contains @, automatically searches by email.

    Examples:
        ashby candidate-search "John Smith"
        ashby candidate-search "john@example.com"
        ashby candidate-search "john@example.com" --email
    """
    client = get_client()
    result = client.search_candidates(query, limit=limit, by_email=by_email)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print(f"[yellow]No candidates found matching '{query}'.[/]")
        raise typer.Exit()

    table = Table(title=f"Search: {query} ({len(result)})")
    table.add_column("ID", style="cyan", max_width=15)
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Email", max_width=35)

    for c in result:
        emails = c.get("emailAddresses", [])
        email = emails[0].get("value", "-") if emails else "-"
        table.add_row(truncate_id(c.get("id")), c.get("name", "-"), email)

    console.print(table)


# ============== Applications ==============


@app.command()
def applications(
    job_id: str = typer.Option(None, "--job", "-j", help="Filter by job ID"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List applications."""
    client = get_client()
    result = client.applications(job_id=job_id, limit=limit)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print("[yellow]No applications found.[/]")
        raise typer.Exit()

    table = Table(title=f"Applications ({len(result)})")
    table.add_column("ID", style="cyan", max_width=15)
    table.add_column("Candidate", style="white", max_width=25)
    table.add_column("Job", max_width=30)
    table.add_column("Stage", style="yellow", max_width=20)
    table.add_column("Status", style="green", max_width=12)

    for a in result:
        cand = a.get("candidate", {})
        job = a.get("job", {})
        stage = a.get("currentInterviewStage", {})
        table.add_row(
            truncate_id(a.get("id")),
            cand.get("name", "-"),
            job.get("title", "-")[:30],
            stage.get("name", "-") if stage else "-",
            a.get("status", "-"),
        )

    console.print(table)


@app.command()
def application(
    application_id: str = typer.Argument(..., help="Application ID"),
    history: bool = typer.Option(False, "--history", "-h", help="Show stage history"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get application details."""
    client = get_client()
    result = client.application(application_id)

    if not result:
        console.print(f"[red]Application '{application_id}' not found.[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    cand = result.get("candidate", {})
    job = result.get("job", {})
    stage = result.get("currentInterviewStage", {})

    console.print("\n[bold cyan]Application[/]")
    console.print(f"ID: {result.get('id', '-')}")
    console.print(f"Candidate: {cand.get('name', '-')}")
    console.print(f"Job: {job.get('title', '-')}")
    console.print(f"Status: [green]{result.get('status', '-')}[/]")
    console.print(f"Stage: [yellow]{stage.get('name', '-') if stage else '-'}[/]")
    console.print(f"Created: {format_date(result.get('createdAt'))}")

    source = result.get("source", {})
    if source:
        console.print(f"Source: {source.get('title', '-')}")

    if history:
        hist = client.application_history(application_id)
        if hist:
            console.print("\n[bold]Stage History:[/]")
            for h in hist:
                stage_name = h.get("interviewStage", {}).get("name", "-")
                entered = format_date(h.get("enteredAt"))
                exited = format_date(h.get("exitedAt"))
                console.print(f"  - {stage_name}: {entered} → {exited}")


# ============== Interviews ==============


@app.command()
def interviews(
    upcoming: bool = typer.Option(False, "--upcoming", "-u", help="Only upcoming interviews"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List interviews."""
    client = get_client()
    result = client.interviews(limit=limit)

    if upcoming:
        now = datetime.now(timezone.utc)
        result = [
            i
            for i in result
            if i.get("startTime")
            and datetime.fromisoformat(i["startTime"].replace("Z", "+00:00")) > now
        ]

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print("[yellow]No interviews found.[/]")
        raise typer.Exit()

    table = Table(title=f"Interviews ({len(result)})")
    table.add_column("ID", style="cyan", max_width=15)
    table.add_column("Candidate", style="white", max_width=25)
    table.add_column("Stage", max_width=20)
    table.add_column("Start Time", style="yellow", max_width=18)
    table.add_column("Interviewers", max_width=25)

    for i in result:
        a = i.get("application", {})
        cand = a.get("candidate", {}) if a else {}
        stage = i.get("interviewStage", {})
        interviewers = i.get("interviewers", [])
        names = ", ".join([iv.get("name", "-") for iv in interviewers[:2]])
        if len(interviewers) > 2:
            names += f" +{len(interviewers) - 2}"

        table.add_row(
            truncate_id(i.get("id")),
            cand.get("name", "-"),
            stage.get("name", "-") if stage else "-",
            format_date(i.get("startTime")),
            names or "-",
        )

    console.print(table)


@app.command()
def interview(
    interview_id: str = typer.Argument(..., help="Interview ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get interview details."""
    client = get_client()
    result = client.interview(interview_id)

    if not result:
        console.print(f"[red]Interview '{interview_id}' not found.[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    a = result.get("application", {})
    cand = a.get("candidate", {}) if a else {}
    stage = result.get("interviewStage", {})

    console.print("\n[bold cyan]Interview[/]")
    console.print(f"ID: {result.get('id', '-')}")
    console.print(f"Candidate: {cand.get('name', '-')}")
    console.print(f"Stage: {stage.get('name', '-') if stage else '-'}")
    console.print(f"Start: {format_date(result.get('startTime'))}")
    console.print(f"End: {format_date(result.get('endTime'))}")

    interviewers = result.get("interviewers", [])
    if interviewers:
        console.print("\n[bold]Interviewers:[/]")
        for iv in interviewers:
            console.print(f"  - {iv.get('name', '-')} ({iv.get('email', '-')})")


# ============== Interview Stages ==============


@app.command()
def stages(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List interview stages."""
    client = get_client()
    result = client.stages()

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print("[yellow]No stages found.[/]")
        raise typer.Exit()

    table = Table(title=f"Interview Stages ({len(result)})")
    table.add_column("ID", style="cyan", max_width=15)
    table.add_column("Name", style="white", max_width=30)
    table.add_column("Type", max_width=15)
    table.add_column("Order", justify="right", max_width=8)

    for s in result:
        table.add_row(
            truncate_id(s.get("id")),
            s.get("name", "-"),
            s.get("type", "-"),
            str(s.get("orderInInterviewPlan", "-")),
        )

    console.print(table)


@app.command("stage-groups")
def stage_groups(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List interview stage groups."""
    client = get_client()
    result = client.stage_groups()

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print("[yellow]No stage groups found.[/]")
        raise typer.Exit()

    table = Table(title=f"Stage Groups ({len(result)})")
    table.add_column("ID", style="cyan", max_width=15)
    table.add_column("Name", style="white", max_width=40)

    for g in result:
        table.add_row(truncate_id(g.get("id")), g.get("name", "-"))

    console.print(table)


# ============== Users ==============


@app.command()
def users(
    limit: int = typer.Option(100, "--limit", "-n", help="Max results"),
    active_only: bool = typer.Option(True, "--active/--all", help="Only show active employees"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List users (active employees by default, use --all for former employees)."""
    client = get_client()
    result = client.users(limit=limit)

    if active_only:
        result = [u for u in result if u.get("isEnabled", False)]

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print("[yellow]No users found.[/]")
        raise typer.Exit()

    table = Table(title=f"Users ({len(result)})")
    table.add_column("ID", style="cyan", max_width=15)
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Email", max_width=35)
    table.add_column("Role", max_width=15)

    for u in result:
        name = f"{u.get('firstName', '')} {u.get('lastName', '')}".strip() or "-"
        table.add_row(truncate_id(u.get("id")), name, u.get("email", "-"), u.get("role", "-"))

    console.print(table)


@app.command()
def user(
    user_id: str = typer.Argument(..., help="User ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get user details."""
    client = get_client()
    result = client.user(user_id)

    if not result:
        console.print(f"[red]User '{user_id}' not found.[/]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    console.print("\n[bold cyan]User[/]")
    console.print(f"ID: {result.get('id', '-')}")
    console.print(f"Name: {result.get('firstName', '')} {result.get('lastName', '')}")
    console.print(f"Email: {result.get('email', '-')}")
    console.print(f"Role: {result.get('role', '-')}")


@app.command()
def me(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get current API key info."""
    client = get_client()
    result = client.api_key_info()

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    console.print("\n[bold cyan]API Key Info[/]")
    console.print(f"Name: {result.get('name', '-')}")
    console.print(f"Active: {result.get('isActive', '-')}")

    permissions = result.get("permissions", {})
    if permissions:
        console.print("\n[bold]Permissions:[/]")
        for module, perms in permissions.items():
            console.print(f"  {module}: {perms}")


@app.command()
def departments(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List departments."""
    client = get_client()
    result = client.departments()

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print("[yellow]No departments found.[/]")
        raise typer.Exit()

    table = Table(title=f"Departments ({len(result)})")
    table.add_column("ID", style="cyan", max_width=15)
    table.add_column("Name", style="white", max_width=30)
    table.add_column("Parent", max_width=25)

    for d in result:
        parent = d.get("parent", {})
        table.add_row(
            truncate_id(d.get("id")),
            d.get("name", "-"),
            parent.get("name", "-") if parent else "-",
        )

    console.print(table)


@app.command()
def sources(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List candidate sources."""
    client = get_client()
    result = client.sources()

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print("[yellow]No sources found.[/]")
        raise typer.Exit()

    table = Table(title=f"Sources ({len(result)})")
    table.add_column("ID", style="cyan", max_width=15)
    table.add_column("Title", style="white", max_width=40)

    for s in result:
        table.add_row(truncate_id(s.get("id")), s.get("title", "-"))

    console.print(table)


@app.command()
def tags(
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List candidate tags."""
    client = get_client()
    result = client.tags()

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print("[yellow]No tags found.[/]")
        raise typer.Exit()

    table = Table(title=f"Candidate Tags ({len(result)})")
    table.add_column("ID", style="cyan", max_width=15)
    table.add_column("Title", style="white", max_width=40)

    for t in result:
        table.add_row(truncate_id(t.get("id")), t.get("title", "-"))

    console.print(table)


# ============== Candidate Screening ==============


@app.command()
def screen(
    job_id: str = typer.Argument(..., help="Job ID to screen applicants for"),
    stage: str = typer.Option(None, "--stage", "-s", help="Filter by stage name (partial match)"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max applications to fetch"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Screen applicants for a job with background info.

    Shows candidate details including position, company, school, and LinkedIn.
    Use this to evaluate candidates for fit.

    Example:
        ashby screen 011f0d77-2afd-4158-aa81-ce6b1e7de4d4 --stage "New"
    """
    client = get_client()

    apps = client.applications(job_id=job_id, limit=limit)

    if not apps:
        console.print(f"[yellow]No applications found for job {job_id}.[/]")
        raise typer.Exit()

    if stage:
        stage_lower = stage.lower()
        apps = [
            a
            for a in apps
            if stage_lower in (a.get("currentInterviewStage", {}).get("name", "") or "").lower()
        ]

    apps = [a for a in apps if not a.get("isArchived", False)]

    if not apps:
        console.print("[yellow]No matching applications found.[/]")
        raise typer.Exit()

    candidates_data = []
    for app in apps:
        cand = app.get("candidate", {})
        if not cand:
            continue

        cand_id = cand.get("id")
        full_candidate = client.candidate(cand_id) if cand_id else {}

        linkedin_url = None
        github_url = None
        for link in full_candidate.get("socialLinks", []):
            link_type = link.get("type", "").lower()
            if "linkedin" in link_type:
                linkedin_url = link.get("url")
            elif "github" in link_type:
                github_url = link.get("url")

        emails = full_candidate.get("emailAddresses", [])
        email = emails[0].get("value", "") if emails else ""

        file_handles = full_candidate.get("fileHandles", [])
        has_resume = any(fh.get("type") == "Resume" for fh in file_handles)

        candidate_info = {
            "id": cand_id,
            "name": full_candidate.get("name", cand.get("name", "-")),
            "email": email,
            "position": full_candidate.get("position"),
            "company": full_candidate.get("company"),
            "school": full_candidate.get("school"),
            "linkedin": linkedin_url,
            "github": github_url,
            "has_resume": has_resume,
            "stage": app.get("currentInterviewStage", {}).get("name", "-"),
            "applied_at": app.get("createdAt"),
            "application_id": app.get("id"),
            "profile_url": full_candidate.get("profileUrl"),
        }
        candidates_data.append(candidate_info)

    if json_output:
        print(json.dumps(candidates_data, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    table = Table(title=f"Applicants ({len(candidates_data)})")
    table.add_column("Name", style="white", max_width=22)
    table.add_column("Position", max_width=20)
    table.add_column("Company", max_width=18)
    table.add_column("School", max_width=18)
    table.add_column("Stage", style="green", max_width=15)
    table.add_column("Links", max_width=8)

    for c in candidates_data:
        links = []
        if c.get("linkedin"):
            links.append("LI")
        if c.get("github"):
            links.append("GH")
        if c.get("has_resume"):
            links.append("CV")

        table.add_row(
            c.get("name", "-"),
            c.get("position", "-") or "-",
            c.get("company", "-") or "-",
            c.get("school", "-") or "-",
            c.get("stage", "-"),
            " ".join(links) if links else "-",
        )

    console.print(table)
    console.print(
        "\n[dim]Use --json for full details including LinkedIn URLs and profile links.[/]"
    )


@app.command("resume-url")
def resume_url(
    candidate_id: str = typer.Argument(..., help="Candidate ID"),
):
    """Get the resume download URL for a candidate."""
    client = get_client()
    url = client.resume_url(candidate_id)

    if not url:
        console.print(f"[yellow]No resume found for candidate {candidate_id}.[/]")
        raise typer.Exit(1)

    print(url)


@app.command("resume-text")
def resume_text(
    candidate_id: str = typer.Argument(..., help="Candidate ID"),
):
    """Extract text from a candidate's resume (requires pypdf)."""
    import logging
    import tempfile
    import warnings

    import httpx

    # Suppress pypdf warnings about malformed PDFs
    logging.getLogger("pypdf").setLevel(logging.ERROR)
    warnings.filterwarnings("ignore", module="pypdf")

    client = get_client()
    url = client.resume_url(candidate_id)

    if not url:
        console.print(f"[yellow]No resume found for candidate {candidate_id}.[/]")
        raise typer.Exit(1)

    try:
        from pypdf import PdfReader
    except ImportError:
        console.print("[red]pypdf not installed. Run: uv add pypdf[/]")
        raise typer.Exit(1)

    # Download the PDF
    response = httpx.get(url, timeout=30.0)
    if response.status_code != 200:
        console.print(f"[red]Failed to download resume: {response.status_code}[/]")
        raise typer.Exit(1)

    # Extract text
    with tempfile.NamedTemporaryFile(suffix=".pdf", delete=True) as tmp:
        tmp.write(response.content)
        tmp.flush()

        reader = PdfReader(tmp.name)
        text_parts = []
        for page in reader.pages:
            text_parts.append(page.extract_text() or "")

    text = "\n".join(text_parts)
    print(text)


@app.command("resume-search")
def resume_search(
    keywords: str = typer.Argument(..., help="Keywords to search for (comma-separated)"),
    job_id: str = typer.Option(None, "--job", "-j", help="Filter by job ID"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max candidates to scan"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search candidate resumes for keywords.

    Downloads and scans resume PDFs for matching keywords.
    This is slow for large candidate pools - use job filter to narrow scope.

    Example: ashby resume-search "crypto,blockchain,defi" --job abc123
    """
    import logging
    import tempfile
    import warnings

    import httpx

    # Suppress pypdf warnings about malformed PDFs
    logging.getLogger("pypdf").setLevel(logging.ERROR)
    warnings.filterwarnings("ignore", module="pypdf")

    try:
        from pypdf import PdfReader
    except ImportError:
        console.print("[red]pypdf not installed. Run: uv add pypdf[/]")
        raise typer.Exit(1)

    client = get_client()
    keyword_list = [k.strip().lower() for k in keywords.split(",")]

    # Get candidates - filter by job if provided
    if job_id:
        apps = client.applications(job_id=job_id, limit=limit)
        candidate_ids = list({a.get("candidateId") for a in apps if a.get("candidateId")})
    else:
        candidates = client.candidates(limit=limit)
        candidate_ids = [c.get("id") for c in candidates if c.get("id")]

    matches = []
    scanned = 0
    no_resume = 0

    console.print(f"[dim]Scanning {len(candidate_ids)} candidates for: {keyword_list}[/]")

    for cid in candidate_ids:
        url = client.resume_url(cid)
        if not url:
            no_resume += 1
            continue

        try:
            response = httpx.get(url, timeout=30.0)
            if response.status_code != 200:
                continue

            with tempfile.NamedTemporaryFile(suffix=".pdf", delete=True) as tmp:
                tmp.write(response.content)
                tmp.flush()

                reader = PdfReader(tmp.name)
                text = ""
                for page in reader.pages:
                    text += (page.extract_text() or "") + "\n"

            text_lower = text.lower()
            found_keywords = [k for k in keyword_list if k in text_lower]

            if found_keywords:
                cand = client.candidate(cid)
                matches.append(
                    {
                        "id": cid,
                        "name": cand.get("name") if cand else "Unknown",
                        "keywords_found": found_keywords,
                        "profile_url": cand.get("profileUrl") if cand else None,
                    }
                )

            scanned += 1

        except Exception:
            continue

    if json_output:
        print(json.dumps(matches, indent=2))
        raise typer.Exit()

    console.print(
        f"\n[bold]Results:[/] {len(matches)} matches from {scanned} scanned ({no_resume} no resume)"
    )

    if matches:
        table = Table(title=f"Resume Matches ({len(matches)})")
        table.add_column("Name", style="white", max_width=25)
        table.add_column("Keywords Found", style="green", max_width=40)
        table.add_column("ID", style="cyan", max_width=15)

        for m in matches:
            table.add_row(
                m["name"],
                ", ".join(m["keywords_found"]),
                truncate_id(m["id"]),
            )

        console.print(table)


# ============== Feedback / Scorecards ==============


def extract_scores_from_feedback(feedback: dict) -> list[dict]:
    """Extract numerical scores from feedback submitted values.

    Returns list of {field_path, score, max_score} dicts.
    Handles various Paradigm feedback formats:
    - ValueSelect with "1", "2", "3", "4" values (overall_recommendation)
    - Score type fields with {"score": N} format
    - Text labels like "Strong Yes", "Yes", "No", "Strong No"
    """
    scores = []
    submitted = feedback.get("submittedValues", {})

    for field_path, value in submitted.items():
        if field_path in ("feedback",) or not value:
            continue

        if isinstance(value, dict) and "score" in value:
            scores.append(
                {
                    "field": field_path,
                    "score": value["score"],
                    "max_score": 4,
                }
            )
        elif isinstance(value, dict) and "value" in value:
            val = value.get("value", "")
            if val in ("Strong No", "No", "Yes", "Strong Yes"):
                score_map = {"Strong No": 1, "No": 2, "Yes": 3, "Strong Yes": 4}
                scores.append(
                    {
                        "field": field_path,
                        "score": score_map.get(val, 0),
                        "max_score": 4,
                        "label": val,
                    }
                )
        elif isinstance(value, str) and value in ("1", "2", "3", "4"):
            label_map = {"1": "Strong No", "2": "No", "3": "Yes", "4": "Strong Yes"}
            scores.append(
                {
                    "field": field_path,
                    "score": int(value),
                    "max_score": 4,
                    "label": label_map.get(value, value),
                }
            )
        elif isinstance(value, int) and 1 <= value <= 4:
            scores.append(
                {
                    "field": field_path,
                    "score": value,
                    "max_score": 4,
                }
            )
    return scores


@app.command()
def feedback(
    application_id: str = typer.Option(None, "--app", "-a", help="Filter by application ID"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max results"),
    active_only: bool = typer.Option(
        True, "--active/--all", help="Only show feedback from active employees"
    ),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List interview feedback/scorecards (from active employees by default)."""
    client = get_client()
    result = client.application_feedback(application_id=application_id, limit=limit)

    if active_only:
        result = [fb for fb in result if fb.get("submittedByUser", {}).get("isEnabled", False)]

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not result:
        console.print("[yellow]No feedback found.[/]")
        raise typer.Exit()

    table = Table(title=f"Interview Feedback ({len(result)})")
    table.add_column("ID", style="cyan", max_width=12)
    table.add_column("Interviewer", style="white", max_width=20)
    table.add_column("Interview", max_width=25)
    table.add_column("Scores", style="yellow", max_width=30)
    table.add_column("Submitted", max_width=16)

    for fb in result:
        user = fb.get("submittedByUser", {})
        interviewer_name = "-"
        if user:
            interviewer_name = f"{user.get('firstName', '')} {user.get('lastName', '')}".strip()

        interview = fb.get("interview", {})
        interview_title = interview.get("title", "-") if interview else "-"

        scores = extract_scores_from_feedback(fb)
        if scores:
            score_strs = []
            for s in scores[:3]:
                if "label" in s:
                    score_strs.append(s["label"])
                else:
                    score_strs.append(f"{s['score']}/{s['max_score']}")
            score_display = ", ".join(score_strs)
            if len(scores) > 3:
                score_display += f" +{len(scores) - 3}"
        else:
            score_display = "-"

        table.add_row(
            truncate_id(fb.get("id")),
            interviewer_name or "-",
            interview_title[:25],
            score_display,
            format_date(fb.get("submittedAt")),
        )

    console.print(table)


@app.command("interviewer-stats")
def interviewer_stats(
    limit: int = typer.Option(500, "--limit", "-n", help="Max feedback to analyze"),
    min_feedback: int = typer.Option(5, "--min", "-m", help="Min feedback count to include"),
    active_only: bool = typer.Option(True, "--active/--all", help="Only include active employees"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Analyze interviewer grading patterns to find harshest/easiest graders.

    Fetches all feedback and calculates average scores per interviewer.
    Use --min to filter out interviewers with few data points.
    Use --all to include former employees (default: active only).
    """
    client = get_client()
    all_feedback = client.application_feedback(limit=limit)

    if not all_feedback:
        console.print("[yellow]No feedback found to analyze.[/]")
        raise typer.Exit()

    interviewer_data: dict[str, dict] = {}

    for fb in all_feedback:
        user = fb.get("submittedByUser", {})
        if not user:
            continue

        if active_only and not user.get("isEnabled", False):
            continue

        user_id = user.get("id")
        user_name = f"{user.get('firstName', '')} {user.get('lastName', '')}".strip()
        user_email = user.get("email", "")

        if user_id not in interviewer_data:
            interviewer_data[user_id] = {
                "name": user_name,
                "email": user_email,
                "total_score": 0,
                "score_count": 0,
                "feedback_count": 0,
                "scores": [],
            }

        interviewer_data[user_id]["feedback_count"] += 1

        scores = extract_scores_from_feedback(fb)
        for s in scores:
            normalized = s["score"] / s["max_score"]
            interviewer_data[user_id]["total_score"] += normalized
            interviewer_data[user_id]["score_count"] += 1
            interviewer_data[user_id]["scores"].append(normalized)

    stats = []
    for uid, data in interviewer_data.items():
        if data["score_count"] > 0 and data["feedback_count"] >= min_feedback:
            avg = data["total_score"] / data["score_count"]
            stats.append(
                {
                    "id": uid,
                    "name": data["name"],
                    "email": data["email"],
                    "avg_score": avg,
                    "feedback_count": data["feedback_count"],
                    "score_count": data["score_count"],
                }
            )

    stats.sort(key=lambda x: x["avg_score"])

    if json_output:
        print(json.dumps(stats, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    if not stats:
        console.print("[yellow]No interviewers with scores found.[/]")
        raise typer.Exit()

    console.print(f"\n[bold]Interviewer Grading Analysis[/] ({len(all_feedback)} feedback entries)")
    console.print("[dim]Sorted by average score (1=Strong No, 2=No, 3=Yes, 4=Strong Yes)[/]\n")

    table = Table()
    table.add_column("Rank", style="cyan", justify="right", max_width=6)
    table.add_column("Interviewer", style="white", max_width=25)
    table.add_column("Avg", style="yellow", justify="right", max_width=8)
    table.add_column("# Feedback", justify="right", max_width=10)

    for i, s in enumerate(stats, 1):
        avg_raw = s["avg_score"] * 4
        table.add_row(
            str(i),
            s["name"] or s["email"],
            f"{avg_raw:.1f}",
            str(s["feedback_count"]),
        )

    console.print(table)

    if stats:
        harshest = stats[0]
        easiest = stats[-1]
        console.print(
            f"\n[bold red]Harshest:[/] {harshest['name']} ({harshest['avg_score'] * 4:.1f}/4)"
        )
        console.print(
            f"[bold blue]Easiest:[/] {easiest['name']} ({easiest['avg_score'] * 4:.1f}/4)"
        )


# ============== Access Control ==============


def normalize_name(name: str) -> str:
    """Normalize a name for comparison (lowercase, strip, remove extra spaces)."""
    if not name:
        return ""
    return " ".join(name.lower().strip().split())


def names_match(name1: str, name2: str) -> bool:
    """Check if two names match (handles first/last name ordering)."""
    n1 = normalize_name(name1)
    n2 = normalize_name(name2)
    if not n1 or not n2:
        return False
    if n1 == n2:
        return True
    parts1 = set(n1.split())
    parts2 = set(n2.split())
    if len(parts1) >= 2 and len(parts2) >= 2:
        return parts1 == parts2
    return False


@app.command("check-access")
def check_access(
    candidate_id: str = typer.Argument(..., help="Candidate ID being queried"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Check if candidate data can be shared.

    Simple rule: If the candidate is a current or past employee, their candidate
    data cannot be shared with anyone, regardless of who is asking.

    Checks performed (any match denies access):
    - Primary email matches an employee email
    - Any secondary email matches an employee email
    - Candidate has @paradigm.xyz email address
    - Exact name match with a current or past employee
    - Last name + first initial match (e.g., "William Berman" matches "Will Berman")

    Exit codes:
        0 = ACCESS GRANTED - candidate is not an employee, data can be shared
        1 = ACCESS DENIED - email or name match, do not share

    Examples:
        ashby check-access 4f701116-b507-439e-b017-d677870a8114
    """
    client = get_client()

    result = {
        "candidate_id": candidate_id,
        "candidate_name": None,
        "candidate_is_employee": False,
        "access_granted": False,
        "reason": "",
    }

    candidate = client.candidate(candidate_id)
    if not candidate:
        result["reason"] = "Candidate not found"
        if json_output:
            print(json.dumps(result, indent=2), file=sys.stdout)
        else:
            console.print(f"[red]ERROR[/]: {result['reason']}")
        raise typer.Exit(1)

    result["candidate_name"] = candidate.get("name", "")

    # Get employees from Ashby
    users = client.all_users()
    employee_emails = {u.get("email", "").lower() for u in users if u.get("email")}
    employee_names = set()
    for u in users:
        name = f"{u.get('firstName', '')} {u.get('lastName', '')}".strip()
        if name:
            employee_names.add(normalize_name(name))

    # Also get employees from pmadmin (more complete list including former employees)
    pmadmin_emails, pmadmin_names = get_pmadmin_employees()
    employee_emails.update(pmadmin_emails)
    employee_names.update(pmadmin_names)

    candidate_emails = set()
    primary = candidate.get("primaryEmailAddress", {})
    if primary:
        candidate_emails.add(primary.get("value", "").lower())
    for email_obj in candidate.get("emailAddresses", []):
        candidate_emails.add(email_obj.get("value", "").lower())

    # Check if any candidate email is @paradigm.xyz - definitive employee indicator
    paradigm_emails = [e for e in candidate_emails if e.endswith("@paradigm.xyz")]
    if paradigm_emails:
        result["candidate_is_employee"] = True
        result["reason"] = (
            f"Candidate has @paradigm.xyz email ({paradigm_emails[0]}) - "
            "their candidate data cannot be shared"
        )
        if json_output:
            print(json.dumps(result, indent=2), file=sys.stdout)
        else:
            console.print(f"[red]ACCESS DENIED[/]: {result['reason']}")
        raise typer.Exit(1)

    if candidate_emails & employee_emails:
        result["candidate_is_employee"] = True
        result["reason"] = (
            "Candidate is a current or past employee - their candidate data cannot be shared"
        )
        if json_output:
            print(json.dumps(result, indent=2), file=sys.stdout)
        else:
            console.print(f"[red]ACCESS DENIED[/]: {result['reason']}")
        raise typer.Exit(1)

    candidate_name_normalized = normalize_name(result["candidate_name"])
    if candidate_name_normalized and candidate_name_normalized in employee_names:
        result["candidate_is_employee"] = True
        result["access_granted"] = False
        result["reason"] = (
            f"Candidate '{result['candidate_name']}' matches a current or past employee name - "
            "their candidate data cannot be shared"
        )
        if json_output:
            print(json.dumps(result, indent=2), file=sys.stdout)
        else:
            console.print(f"[red]ACCESS DENIED[/]: {result['reason']}")
        raise typer.Exit(1)

    # Check for last name + first name initial match (e.g., "William Berman" vs "Will Berman")
    candidate_parts = candidate_name_normalized.split() if candidate_name_normalized else []
    if len(candidate_parts) >= 2:
        candidate_last = candidate_parts[-1]
        candidate_first_initial = candidate_parts[0][0] if candidate_parts[0] else ""
        for emp_name in employee_names:
            emp_parts = emp_name.split()
            if len(emp_parts) >= 2:
                emp_last = emp_parts[-1]
                emp_first_initial = emp_parts[0][0] if emp_parts[0] else ""
                # Match if same last name and same first initial
                if candidate_last == emp_last and candidate_first_initial == emp_first_initial:
                    result["candidate_is_employee"] = True
                    result["access_granted"] = False
                    result["reason"] = (
                        f"Candidate '{result['candidate_name']}' likely matches employee - "
                        "same last name and first initial - their candidate data cannot be shared"
                    )
                    if json_output:
                        print(json.dumps(result, indent=2), file=sys.stdout)
                    else:
                        console.print(f"[red]ACCESS DENIED[/]: {result['reason']}")
                    raise typer.Exit(1)

    result["access_granted"] = True
    result["reason"] = "Candidate is not a current or past employee - data can be shared"

    if json_output:
        print(json.dumps(result, indent=2), file=sys.stdout)
    else:
        console.print(f"[green]ACCESS GRANTED[/]: {result['reason']}")

    raise typer.Exit(0)


if __name__ == "__main__":
    app()
