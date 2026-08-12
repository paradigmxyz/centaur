alter table feishu_deliveries
    add column desired_version integer not null default 0,
    add constraint feishu_deliveries_desired_version_nonnegative
        check (desired_version >= 0);

update feishu_deliveries set desired_version = render_version;

alter table feishu_deliveries
    add constraint feishu_deliveries_render_not_ahead
        check (render_version <= desired_version);

create or replace function wake_feishu_delivery_for_session_event()
returns trigger
language plpgsql
as $$
begin
    update feishu_deliveries
       set desired_version = desired_version + 1,
           state = 'pending', failure_code = null, updated_at = now()
     where thread_key = new.thread_key;
    return new;
end;
$$;

create trigger session_events_wake_feishu_delivery
after insert on session_events
for each row execute function wake_feishu_delivery_for_session_event();

create or replace function wake_feishu_delivery_for_selection()
returns trigger
language plpgsql
as $$
begin
    update feishu_deliveries delivery
       set desired_version = delivery.desired_version + 1,
           state = 'pending', failure_code = null, updated_at = now()
      from session_workspaces workspace
     where workspace.workspace_id = new.workspace_id
       and delivery.thread_key = workspace.thread_key;
    return new;
end;
$$;

create trigger development_selection_wakes_feishu_delivery
after update on development_selection_flows
for each row execute function wake_feishu_delivery_for_selection();
