-- Slack delivery now uses the owning workflow's ordinary checkpoints.
-- Deadlines and terminal owners are checked on wait and invocation.
drop index if exists workflow_actions_pending_idx;
drop index if exists workflow_actions_delivery_idx;
alter table workflow_actions
    drop column if exists message_ts,
    drop column if exists delivered_state,
    drop column if exists delivery_lease,
    drop column if exists delivery_after,
    drop column if exists checked_at;
