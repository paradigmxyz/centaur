---
name: scheduled-tasks
description: "Create, list, read, update, delete, or run Centaur scheduled tasks with `centaur-console`. Use when a user asks to schedule recurring or one-off agent work, such as \"every Monday at 9 summarize #channel\", \"remind me daily\", \"run this report weekly\", or to pause, change, delete, or trigger an existing task."
---

# Scheduled Tasks

Manage recurring or ad-hoc agent work with `centaur-console`. Run `centaur-console <command> --help` before using an unfamiliar option.

```bash
centaur-console tasks                                      # list tasks owned by this user or channel
centaur-console task <task-id>
centaur-console create-task <name> --prompt "<prompt>" [--cron "<m h dom mon dow>"] [--delivery-channel dm|<C…>] [--enabled/--disabled]
centaur-console update-task <task-id> [--name …] [--prompt …] [--cron "…"|""] [--delivery-channel …] [--enabled/--disabled]
centaur-console delete-task <task-id>
centaur-console run-task <task-id>                         # queue an immediate run
```

## Ownership and delivery

- In MCP or direct-message (DM) sessions, tasks belong to the user and deliver to `dm` (the default) or an allowed Slack channel ID.
- In a Slack channel thread, tasks belong to that channel: they run with the channel's permissions, always post to it (omit `--delivery-channel`), and anyone in the channel can manage them.

## Schedules and prompts

- Schedules are optional five-field cron expressions in Pacific Time. Omit `--cron` for an ad-hoc task triggered with `run-task`; pass `--cron ""` to `update-task` to clear a schedule.
- Encode recurrence only in the cron expression. Strip cadence phrases like "Each Monday" from the prompt, but keep time windows that affect the work, such as "messages from the past week".
- Write the prompt so it stands alone: the task runs in a fresh session without this conversation's context.

## After a change

Report the task ID, schedule, destination, enabled state, and next run. Do not repeat a successful mutation; if a call's outcome is unclear, check with `tasks` or `task <id>` before retrying.
