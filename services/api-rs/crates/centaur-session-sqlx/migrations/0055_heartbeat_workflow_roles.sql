do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_heartbeat_run') then
        create role centaur_heartbeat_run nologin;
    end if;
    if not exists (select 1 from pg_roles where rolname = 'centaur_heartbeat_feedback') then
        create role centaur_heartbeat_feedback nologin;
    end if;
end
$$;

grant usage on schema public to centaur_heartbeat_run, centaur_heartbeat_feedback;
grant centaur_heartbeat_run, centaur_heartbeat_feedback to current_user;

grant select, insert, update, delete on
    heartbeat_profiles,
    heartbeat_profile_grants,
    heartbeat_runs,
    heartbeat_source_checkpoints,
    heartbeat_observations,
    heartbeat_items,
    heartbeat_item_observations,
    heartbeat_item_events,
    heartbeat_run_artifacts,
    heartbeat_deliveries,
    heartbeat_action_tokens,
    memory_facts,
    memory_fact_evidence,
    memory_fact_events
to centaur_heartbeat_run;

grant select on
    heartbeat_action_tokens,
    heartbeat_deliveries,
    heartbeat_runs,
    heartbeat_profile_grants,
    heartbeat_items
to centaur_heartbeat_feedback;

grant update on heartbeat_action_tokens, heartbeat_items
to centaur_heartbeat_feedback;

grant insert on heartbeat_item_events
to centaur_heartbeat_feedback;
