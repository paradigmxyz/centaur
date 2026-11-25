---
name: scheduled-tasks
description: "Create, list, read, update, delete, or run Centaur scheduled tasks with the `centaur_console` Code Mode connector. Use when a user asks to schedule recurring or one-off agent work, such as \"every Monday at 9 summarize #channel\", \"remind me daily\", \"run this report weekly\", or to pause, change, delete, or trigger an existing task."
---

# Scheduled Tasks

Manage recurring or ad-hoc agent work with the `centaur_console` connector in Code Mode (`centaur-codemode exec --result-only`). `centaur-codemode load centaur_console` prints every function and its arguments.

```js
const cc = tools.centaur_console;
await cc.scheduled_tasks();                                   // tasks owned by this user or channel
await cc.scheduled_task({ task_id: "tsk_..." });
await cc.create_scheduled_task({ name, prompt, cron_expression, delivery_channel, enabled });
await cc.update_scheduled_task({ task_id, name, prompt, cron_expression, delivery_channel, enabled });
await cc.delete_scheduled_task({ task_id });
await cc.run_scheduled_task({ task_id });                     // queue an immediate run
```

Only `name` and `prompt` are required to create a task; omitted fields on update are kept.

## Ownership and delivery

- In MCP or direct-message (DM) sessions, tasks belong to the user and deliver to `dm` (the default when `delivery_channel` is omitted) or an allowed Slack channel ID.
- In a Slack channel thread, tasks belong to that channel by default: they run with the channel's permissions, always post to it (omit `delivery_channel`), and anyone in the channel can manage them.
- When someone in a channel thread wants the task for themselves ("remind me", "DM me", "send me"), pass `owner: "me"` to `create_scheduled_task` and omit `delivery_channel`. The task belongs to the person who sent the current message, runs with their permissions, and always delivers to their DM. Nobody can create a task for, or deliver it to, another person; say so if asked. Personal tasks cannot be listed or changed from the channel, so tell the user to manage them from their DM.
- If it is unclear whether the user wants a channel task or a personal one, ask before creating it.

## Schedules and prompts

- Schedules are optional five-field cron expressions in Pacific Time. Omit `cron_expression` for an ad-hoc task triggered with `run_scheduled_task`; pass `cron_expression: ""` to `update_scheduled_task` to clear a schedule.
- Encode recurrence only in the cron expression. Strip cadence phrases like "Each Monday" from the prompt, but keep time windows that affect the work, such as "messages from the past week".
- Write the prompt so it stands alone: the task runs in a fresh session without this conversation's context.

## After a change

Report the task ID, schedule, destination, enabled state, and next run. Do not repeat a successful mutation; if a call's outcome is unclear, check with `scheduled_tasks` or `scheduled_task` before retrying.
