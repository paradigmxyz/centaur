-- Slack calls now wait for their slots in place and honor only Retry-After,
-- so the schedule no longer widens on rate limits. Slots reserved far ahead by
-- suspended tasks are released so new reservations start from now.
alter table company_context_system.slack_rate_limits
    drop column backoff,
    drop column backoff_adjusted_at;

update company_context_system.slack_rate_limits
set next_slot_at = least(next_slot_at, now()),
    updated_at = now();
