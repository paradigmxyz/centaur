---
name: create-linear-project
description: "Create a Linear project under the Darkbloom team through a required intake flow. Use when an admin asks to create/start/spin up a new Linear project, a new bet, or a new initiative to go after. Enforces a structured questionnaire (hypothesis, metrics, evidence, impact timing, priority, lead, dates) before anything is created."
---

# Create Linear Project (Darkbloom)

We don't just create projects — we run a flow. Every new Darkbloom project must answer a
fixed set of questions before it exists. This skill enforces that intake, then creates the
project directly in Linear with no preview step.

## Who can use this

Project creation is a **mutation** — admins only (see the access policy). If a non-admin
asks, decline per the standard read-only message. Do not run any step below for them.

## The intake (all nine fields are required)

Present this template verbatim on first invocation, then let the user reply freeform. They
will not answer in template form — **you** slot their answers into the fields and keep
asking only for what's still missing. Do not proceed until every field is filled.

```
═══ NEW LINEAR PROJECT — INTAKE (Darkbloom) ═══

1. What is this?          — one-paragraph overview of the project
2. Hypothesis             — the specific bet we're making
3. Metrics to improve     — the full set of metrics we expect this to move
4. Evidence               — what gives us direction; why this hypothesis is worth pursuing
5. Impact timing          — do we expect immediate movement? if not, when does meaningful
                            improvement land?
6. Priority               — Urgent | High | Medium | Low
7. Lead                   — who owns this?
8. Start date             — YYYY-MM-DD
9. Target date            — YYYY-MM-DD
```

Notes on gathering:
- Fields 1 and 2 are **distinct**: "What is this?" is the overview; "Hypothesis" is the bet.
- Keep a running tally. After each user reply, restate what's captured and list what's
  still outstanding. Ask targeted follow-ups for the gaps — don't re-ask filled fields.
- If a date is given relatively ("end of Q3"), resolve it to a concrete `YYYY-MM-DD` and
  confirm the resolved value.

## Mapping to Linear

- **Team**: always Darkbloom — `teamIds: ["120662cb-3a74-4b46-8105-a80adee59391"]` (key `DAR`).
  If you want to avoid hardcoding the id, resolve it live: `call linear teams` → the `DAR` row.
- **Native fields**: priority → `priority`, lead → `lead_id`, start → `start_date`,
  target → `target_date`.
- **Fields 1–5 → the project overview document** (`content`), as the markdown template below.
- **`description`**: a one-line summary derived from field 1 (Linear caps it at 255 chars).

### Priority mapping

| Label  | value |
|--------|-------|
| Urgent | 1 |
| High   | 2 |
| Medium | 3 |
| Low    | 4 |

### Lead resolution

Resolve the lead name to a user id: `call linear users`, match on name, take `id`. If the
name is ambiguous or unmatched, ask the user to disambiguate — don't guess.

### `content` template (fields 1–5)

```markdown
## What is this?
{field 1}

## Hypothesis
{field 2}

## Metrics we expect to improve
{field 3}

## Evidence
{field 4}

## Expected impact timing
{field 5}
```

## Create (directly — no preview)

Once all nine fields are captured and the lead/team ids are resolved, create immediately:

```bash
call linear create_project '{
  "name": "<project name>",
  "team_ids": ["120662cb-3a74-4b46-8105-a80adee59391"],
  "description": "<one-line summary from field 1>",
  "content": "<the markdown template above>",
  "lead_id": "<resolved user id>",
  "priority": <1-4>,
  "start_date": "YYYY-MM-DD",
  "target_date": "YYYY-MM-DD"
}'
```

Then confirm with the returned project `url`. If the call fails, report the error and the
captured intake so nothing is lost — do not silently retry a mutation.

## Guardrails

- Never create with missing intake fields, even if the user says "just make it."
- Do not render a confirmation preview — the user has opted into direct creation.
- Don't create duplicate projects on retry: if a `create_project` call returned a project,
  treat it as done even if downstream formatting failed.
