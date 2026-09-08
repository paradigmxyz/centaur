-- A group of buttons resumes its owning workflow exactly once. Delivery is
-- retried independently of the recorded choice and workflow execution.
create table workflow_actions (
    id uuid primary key,
    queue_name text not null,
    task_id uuid not null,
    step_name text not null,
    workflow_name text not null,
    config jsonb not null,
    expires_at timestamptz not null,
    state text not null default 'pending'
        check (state in ('pending', 'resolved', 'expired', 'cancelled')),
    result jsonb,
    message_ts text,
    delivered_state text,
    delivery_lease uuid,
    delivery_after timestamptz not null default now(),
    created_at timestamptz not null default now(),
    checked_at timestamptz not null default now(),
    resolved_at timestamptz,
    unique (queue_name, task_id, step_name),
    check ((state = 'pending') = (result is null))
);

create index workflow_actions_pending_idx on workflow_actions (checked_at)
    where state = 'pending';
create index workflow_actions_delivery_idx on workflow_actions (delivery_after)
    where delivered_state is distinct from state;
