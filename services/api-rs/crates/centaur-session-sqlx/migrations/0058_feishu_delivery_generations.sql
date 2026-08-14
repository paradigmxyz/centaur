alter table feishu_deliveries
    add column delivery_generation integer not null default 0,
    add column execution_id text references session_executions(execution_id) on delete set null,
    add column selection_flow_id text references development_selection_flows(selection_flow_id) on delete set null,
    add constraint feishu_deliveries_generation_nonnegative
        check (delivery_generation >= 0);

create table development_selection_receipts (
    thread_key text not null references sessions(thread_key) on delete cascade,
    idempotency_key text not null,
    source_message_id text not null,
    selection_flow_id text not null references development_selection_flows(selection_flow_id) on delete cascade,
    created_at timestamptz not null default now(),
    primary key (thread_key, idempotency_key),
    unique (thread_key, source_message_id)
);

with delivery_targets as (
    select delivery.thread_key,
           selection.selection_flow_id,
           selection.execution_id as selection_execution_id,
           selection.updated_at as selection_updated_at,
           execution.execution_id,
           execution.created_at as execution_created_at
      from feishu_deliveries delivery
      left join lateral (
          select flow.selection_flow_id, flow.execution_id, flow.updated_at
            from development_selection_flows flow
            join session_workspaces workspace using (workspace_id)
           where workspace.thread_key = delivery.thread_key
           order by flow.updated_at desc, flow.selection_flow_id desc
           limit 1
      ) selection on true
      left join lateral (
          select candidate.execution_id, candidate.created_at
            from session_executions candidate
           where candidate.thread_key = delivery.thread_key
           order by candidate.created_at desc, candidate.execution_id desc
           limit 1
      ) execution on true
)
update feishu_deliveries delivery
   set selection_flow_id = case
           when target.selection_updated_at is not null
                and (target.execution_created_at is null
                     or target.selection_updated_at >= target.execution_created_at)
             then target.selection_flow_id
           else null
       end,
       execution_id = case
           when target.selection_updated_at is not null
                and (target.execution_created_at is null
                     or target.selection_updated_at >= target.execution_created_at)
             then target.selection_execution_id
           else target.execution_id
       end
  from delivery_targets target
 where target.thread_key = delivery.thread_key;

create or replace function wake_feishu_delivery_for_session_event()
returns trigger
language plpgsql
as $$
begin
    if new.execution_id is not null then
        update feishu_deliveries
           set desired_version = desired_version + 1,
               state = 'pending', failure_code = null, updated_at = now()
         where thread_key = new.thread_key
           and execution_id = new.execution_id;
    end if;
    return new;
end;
$$;

create or replace function wake_feishu_delivery_for_selection()
returns trigger
language plpgsql
as $$
begin
    update feishu_deliveries
       set desired_version = desired_version + 1,
           state = 'pending', failure_code = null, updated_at = now()
     where selection_flow_id = new.selection_flow_id;
    return new;
end;
$$;
