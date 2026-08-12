alter table development_selection_flows
    add column task_excerpt text not null default '',
    add column query text not null default '',
    add column cursor text,
    add column cursor_history jsonb not null default '[]'::jsonb,
    add constraint development_selection_flows_cursor_history_array
        check (jsonb_typeof(cursor_history) = 'array');

alter table feishu_deliveries
    add column lease_owner text,
    add column lease_expires_at timestamptz,
    add constraint feishu_deliveries_lease_complete check (
        (lease_owner is null) = (lease_expires_at is null)
    );

create index feishu_deliveries_reconcile_idx
    on feishu_deliveries (state, lease_expires_at, updated_at)
    where state in ('pending', 'failed');
