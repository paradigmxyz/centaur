-- iron-proxy sets this session value from the assigned principal's immutable
-- foreign_id while opening the pg_dsn connection. A custom GUC is writable by
-- any PostgreSQL client, so it is only an assertion. The owner-controlled
-- binding below ties that assertion to the fixed connection role that the
-- proxy assigns. A missing binding, or a value different from the binding,
-- fails closed.
create table if not exists heartbeat_workflow_principal_bindings (
    database_role name primary key,
    workflow_principal text not null unique,
    created_at timestamptz not null default now()
);

revoke all on heartbeat_workflow_principal_bindings from public;
revoke all on heartbeat_workflow_principal_bindings from centaur_heartbeat_run;
revoke all on heartbeat_workflow_principal_bindings from centaur_heartbeat_feedback;

-- FORCE RLS also applies to table owners. Keep the feedback facade's
-- definer role non-login and narrowly grant it the heartbeat tables it needs;
-- it is the only intentional BYPASSRLS path and is not a client role.
do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_heartbeat_definer') then
        create role centaur_heartbeat_definer nologin bypassrls;
    end if;
end
$$;
grant usage on schema public to centaur_heartbeat_definer;
grant select, insert, update on heartbeat_workflow_principal_bindings
    to centaur_heartbeat_definer;
grant select, insert, update, delete on
    heartbeat_profiles, heartbeat_profile_grants, heartbeat_runs,
    heartbeat_items, heartbeat_item_events, heartbeat_deliveries,
    heartbeat_action_tokens, memory_facts, memory_fact_evidence,
    memory_fact_events
to centaur_heartbeat_definer;

alter table memory_facts
    add column if not exists owner_principal text;

-- Preserve any pre-RLS proposal/correction history instead of making it
-- silently disappear behind the new owner policy. Root proposals were written
-- by the workflow principal; corrected descendants inherit that same owner.
with recursive memory_owners as (
    select fact_id,
           nullif(btrim(coalesce(proposed_by_principal, confirmed_by_principal)), '') as owner_principal
      from memory_facts
     where supersedes_fact_id is null
    union all
    select child.fact_id, parent.owner_principal
      from memory_facts child
      join memory_owners parent on parent.fact_id = child.supersedes_fact_id
)
update memory_facts fact
   set owner_principal = owners.owner_principal
  from memory_owners owners
 where fact.fact_id = owners.fact_id
   and fact.owner_principal is null;

do $$
begin
    if exists (select 1 from memory_facts where owner_principal is null) then
        raise exception 'cannot enable heartbeat memory RLS: existing fact owner is unknown';
    end if;
end
$$;

alter table memory_facts alter column owner_principal set not null;

create or replace function memory_fact_owner_guard()
returns trigger language plpgsql security definer
set search_path = pg_catalog, public
as $$
begin
    if new.owner_principal is null or btrim(new.owner_principal) = '' then
        raise exception 'memory fact owner_principal is required' using errcode = '23514';
    end if;
    if tg_op = 'UPDATE' and new.owner_principal is distinct from old.owner_principal then
        raise exception 'memory fact owner_principal is immutable' using errcode = '27000';
    end if;
    return new;
end;
$$;

alter function memory_fact_owner_guard() owner to centaur_heartbeat_definer;
revoke all on function memory_fact_owner_guard() from public;
drop trigger if exists memory_fact_owner_guard on memory_facts;
create trigger memory_fact_owner_guard
    before insert or update on memory_facts
    for each row execute function memory_fact_owner_guard();

-- The deployment/operator role calls this once for each fixed proxy database
-- role. Workflow roles cannot grant themselves a binding or alter one.
create or replace function heartbeat_bind_workflow_principal(
    p_database_role name, p_workflow_principal text
) returns void language sql security definer
set search_path = pg_catalog, public
as $$
    insert into public.heartbeat_workflow_principal_bindings
        (database_role, workflow_principal)
    values ($1, $2)
    on conflict (database_role) do update
        set workflow_principal = excluded.workflow_principal
$$;

alter function heartbeat_bind_workflow_principal(name, text)
    owner to centaur_heartbeat_definer;

revoke all on function heartbeat_bind_workflow_principal(name, text) from public;

create or replace function heartbeat_current_workflow_principal()
returns text
language sql
stable
security definer
set search_path = pg_catalog, public
as $$
    select b.workflow_principal
      from public.heartbeat_workflow_principal_bindings b
     where b.database_role = current_setting('role')::name
       and b.workflow_principal = nullif(current_setting('centaur.workflow_principal', true), '')
$$;

alter function heartbeat_current_workflow_principal()
    owner to centaur_heartbeat_definer;

create or replace function heartbeat_profile_visible(p_profile_id uuid)
returns boolean
language sql
stable
security definer
set search_path = pg_catalog, public
as $$
    select exists (
        select 1 from public.heartbeat_profiles p
        where p.profile_id = p_profile_id
          and p.executor_principal_foreign_id = (
              select b.workflow_principal
                from public.heartbeat_workflow_principal_bindings b
               where b.database_role = current_setting('role')::name
                 and b.workflow_principal = nullif(current_setting('centaur.workflow_principal', true), '')
          )
    )
$$;

alter function heartbeat_profile_visible(uuid)
    owner to centaur_heartbeat_definer;

create or replace function heartbeat_memory_scope_visible(
    p_namespace text, p_scope_kind text, p_scope_ref text
)
returns boolean
language sql
stable
security definer
set search_path = pg_catalog, public
as $$
    select exists (
        select 1 from public.heartbeat_profiles p
        where p.namespace = p_namespace
          and p.scope_kind = p_scope_kind
          and p.scope_ref = p_scope_ref
          and p.executor_principal_foreign_id = (
              select b.workflow_principal
                from public.heartbeat_workflow_principal_bindings b
               where b.database_role = current_setting('role')::name
                 and b.workflow_principal = nullif(current_setting('centaur.workflow_principal', true), '')
          )
    )
$$;

alter function heartbeat_memory_scope_visible(text, text, text)
    owner to centaur_heartbeat_definer;

create or replace function heartbeat_run_visible(p_run_id uuid)
returns boolean language sql stable security definer
set search_path = pg_catalog, public
as $$
    select exists (
        select 1 from public.heartbeat_runs r
         where r.run_id = p_run_id
           and r.executor_principal_foreign_id = heartbeat_current_workflow_principal()
    )
$$;

alter function heartbeat_run_visible(uuid)
    owner to centaur_heartbeat_definer;

create or replace function heartbeat_item_visible(p_item_id uuid)
returns boolean language sql stable security definer
set search_path = pg_catalog, public
as $$
    select exists (
        select 1 from public.heartbeat_items i
         where i.item_id = p_item_id
           and heartbeat_profile_visible(i.profile_id)
    )
$$;

alter function heartbeat_item_visible(uuid)
    owner to centaur_heartbeat_definer;

create or replace function heartbeat_delivery_visible(p_delivery_id uuid)
returns boolean language sql stable security definer
set search_path = pg_catalog, public
as $$
    select exists (
        select 1 from public.heartbeat_deliveries d
         where d.delivery_id = p_delivery_id
           and heartbeat_run_visible(d.run_id)
    )
$$;

alter function heartbeat_delivery_visible(uuid)
    owner to centaur_heartbeat_definer;

create or replace function heartbeat_memory_fact_visible(p_fact_id uuid)
returns boolean language sql stable security definer
set search_path = pg_catalog, public
as $$
    select exists (
        select 1 from public.memory_facts f
         where f.fact_id = p_fact_id
           and f.owner_principal = heartbeat_current_workflow_principal()
    )
$$;

alter function heartbeat_memory_fact_visible(uuid)
    owner to centaur_heartbeat_definer;

revoke all on function heartbeat_current_workflow_principal() from public;
revoke all on function heartbeat_profile_visible(uuid) from public;
revoke all on function heartbeat_memory_scope_visible(text, text, text) from public;
revoke all on function heartbeat_run_visible(uuid) from public;
revoke all on function heartbeat_item_visible(uuid) from public;
revoke all on function heartbeat_delivery_visible(uuid) from public;
revoke all on function heartbeat_memory_fact_visible(uuid) from public;
grant execute on function heartbeat_current_workflow_principal() to centaur_heartbeat_run, centaur_heartbeat_feedback;
grant execute on function heartbeat_profile_visible(uuid) to centaur_heartbeat_run, centaur_heartbeat_feedback;
grant execute on function heartbeat_memory_scope_visible(text, text, text) to centaur_heartbeat_run, centaur_heartbeat_feedback;
grant execute on function heartbeat_run_visible(uuid) to centaur_heartbeat_run, centaur_heartbeat_feedback;
grant execute on function heartbeat_item_visible(uuid) to centaur_heartbeat_run, centaur_heartbeat_feedback;
grant execute on function heartbeat_delivery_visible(uuid) to centaur_heartbeat_run, centaur_heartbeat_feedback;
grant execute on function heartbeat_memory_fact_visible(uuid) to centaur_heartbeat_run, centaur_heartbeat_feedback;
grant execute on function heartbeat_current_workflow_principal() to centaur_heartbeat_definer;
grant execute on function heartbeat_profile_visible(uuid) to centaur_heartbeat_definer;
grant execute on function heartbeat_memory_scope_visible(text, text, text) to centaur_heartbeat_definer;
grant execute on function heartbeat_run_visible(uuid) to centaur_heartbeat_definer;
grant execute on function heartbeat_item_visible(uuid) to centaur_heartbeat_definer;
grant execute on function heartbeat_delivery_visible(uuid) to centaur_heartbeat_definer;
grant execute on function heartbeat_memory_fact_visible(uuid) to centaur_heartbeat_definer;

create or replace function heartbeat_replace_profile_grants(
    p_profile_id uuid, p_executor_principal text, p_reviewers text[]
) returns void language plpgsql security definer
set search_path = pg_catalog, public
as $$
declare reviewer text;
begin
    if p_executor_principal is null
       or p_executor_principal <> heartbeat_current_workflow_principal()
       or not exists (
           select 1 from public.heartbeat_profiles p
            where p.profile_id = p_profile_id
              and p.executor_principal_foreign_id = p_executor_principal
       ) then
        raise exception 'workflow principal cannot modify heartbeat profile grants'
            using errcode = '42501';
    end if;
    delete from public.heartbeat_profile_grants
     where profile_id = p_profile_id and permission = 'review';
    foreach reviewer in array coalesce(p_reviewers, '{}'::text[]) loop
        insert into public.heartbeat_profile_grants (
            profile_id, subject_kind, subject_ref, permission, granted_by_principal
        ) values (p_profile_id, 'principal', reviewer, 'review', p_executor_principal);
    end loop;
    delete from public.heartbeat_profile_grants
     where profile_id = p_profile_id and permission = 'operate';
    insert into public.heartbeat_profile_grants (
        profile_id, subject_kind, subject_ref, permission, granted_by_principal
    ) values (p_profile_id, 'principal', p_executor_principal, 'operate', p_executor_principal);
end;
$$;

alter function heartbeat_replace_profile_grants(uuid, text, text[])
    owner to centaur_heartbeat_definer;

revoke all on function heartbeat_replace_profile_grants(uuid, text, text[]) from public;
grant execute on function heartbeat_replace_profile_grants(uuid, text, text[])
    to centaur_heartbeat_run;

drop policy if exists heartbeat_profiles_principal_scope on heartbeat_profiles;
drop policy if exists heartbeat_profile_grants_principal_scope on heartbeat_profile_grants;
drop policy if exists heartbeat_runs_principal_scope on heartbeat_runs;
drop policy if exists heartbeat_source_checkpoints_principal_scope on heartbeat_source_checkpoints;
drop policy if exists heartbeat_observations_principal_scope on heartbeat_observations;
drop policy if exists heartbeat_items_principal_scope on heartbeat_items;
drop policy if exists heartbeat_item_observations_principal_scope on heartbeat_item_observations;
drop policy if exists heartbeat_item_events_principal_scope on heartbeat_item_events;
drop policy if exists heartbeat_run_artifacts_principal_scope on heartbeat_run_artifacts;
drop policy if exists heartbeat_deliveries_principal_scope on heartbeat_deliveries;
drop policy if exists heartbeat_action_tokens_principal_scope on heartbeat_action_tokens;
drop policy if exists memory_facts_principal_scope on memory_facts;
drop policy if exists memory_fact_evidence_principal_scope on memory_fact_evidence;
drop policy if exists memory_fact_events_principal_scope on memory_fact_events;

alter table heartbeat_profiles enable row level security;
alter table heartbeat_profiles force row level security;
alter table heartbeat_profile_grants enable row level security;
alter table heartbeat_profile_grants force row level security;
alter table heartbeat_runs enable row level security;
alter table heartbeat_runs force row level security;
alter table heartbeat_source_checkpoints enable row level security;
alter table heartbeat_source_checkpoints force row level security;
alter table heartbeat_observations enable row level security;
alter table heartbeat_observations force row level security;
alter table heartbeat_items enable row level security;
alter table heartbeat_items force row level security;
alter table heartbeat_item_observations enable row level security;
alter table heartbeat_item_observations force row level security;
alter table heartbeat_item_events enable row level security;
alter table heartbeat_item_events force row level security;
alter table heartbeat_run_artifacts enable row level security;
alter table heartbeat_run_artifacts force row level security;
alter table heartbeat_deliveries enable row level security;
alter table heartbeat_deliveries force row level security;
alter table heartbeat_action_tokens enable row level security;
alter table heartbeat_action_tokens force row level security;
alter table memory_facts enable row level security;
alter table memory_facts force row level security;
alter table memory_fact_evidence enable row level security;
alter table memory_fact_evidence force row level security;
alter table memory_fact_events enable row level security;
alter table memory_fact_events force row level security;

create policy heartbeat_profiles_principal_scope on heartbeat_profiles
    using (executor_principal_foreign_id = heartbeat_current_workflow_principal())
    with check (executor_principal_foreign_id = heartbeat_current_workflow_principal());

create policy heartbeat_profile_grants_principal_scope on heartbeat_profile_grants
    using (heartbeat_profile_visible(profile_id))
    with check (heartbeat_profile_visible(profile_id));

create policy heartbeat_runs_principal_scope on heartbeat_runs
    using (executor_principal_foreign_id = heartbeat_current_workflow_principal())
    with check (executor_principal_foreign_id = heartbeat_current_workflow_principal());

create policy heartbeat_source_checkpoints_principal_scope on heartbeat_source_checkpoints
    using (heartbeat_profile_visible(profile_id))
    with check (heartbeat_profile_visible(profile_id));

create policy heartbeat_observations_principal_scope on heartbeat_observations
    using (heartbeat_profile_visible(profile_id))
    with check (heartbeat_profile_visible(profile_id));

create policy heartbeat_items_principal_scope on heartbeat_items
    using (heartbeat_profile_visible(profile_id))
    with check (heartbeat_profile_visible(profile_id));

create policy heartbeat_item_observations_principal_scope on heartbeat_item_observations
    using (heartbeat_item_visible(item_id))
    with check (heartbeat_item_visible(item_id));

create policy heartbeat_item_events_principal_scope on heartbeat_item_events
    using (heartbeat_item_visible(item_id))
    with check (heartbeat_item_visible(item_id));

create policy heartbeat_run_artifacts_principal_scope on heartbeat_run_artifacts
    using (heartbeat_run_visible(run_id))
    with check (heartbeat_run_visible(run_id));

create policy heartbeat_deliveries_principal_scope on heartbeat_deliveries
    using (heartbeat_run_visible(run_id))
    with check (heartbeat_run_visible(run_id));

create policy heartbeat_action_tokens_principal_scope on heartbeat_action_tokens
    using (heartbeat_delivery_visible(delivery_id))
    with check (heartbeat_delivery_visible(delivery_id));

create policy memory_facts_principal_scope on memory_facts
    using (owner_principal = heartbeat_current_workflow_principal())
    with check (owner_principal = heartbeat_current_workflow_principal());

create policy memory_fact_evidence_principal_scope on memory_fact_evidence
    using (heartbeat_memory_fact_visible(fact_id))
    with check (heartbeat_memory_fact_visible(fact_id));

create policy memory_fact_events_principal_scope on memory_fact_events
    using (heartbeat_memory_fact_visible(fact_id))
    with check (heartbeat_memory_fact_visible(fact_id));

-- Feedback consumes a one-time token through this narrow SECURITY DEFINER
-- boundary. Its SQL role has no row policy and therefore cannot enumerate or
-- mutate another profile's heartbeat state directly.
create or replace function heartbeat_consume_action(
    p_token_hash text,
    p_actor_ref text,
    p_provider_event_key text
) returns jsonb
language plpgsql
security definer
set search_path = pg_catalog, public
as $$
declare
    token_row record;
    item_row record;
    allowed boolean;
    to_status text;
    disposition_value text;
    snooze_value timestamptz;
    new_version integer;
    event_key text;
begin
    if nullif(trim(p_token_hash), '') is null
       or nullif(trim(p_actor_ref), '') is null
       or nullif(trim(p_provider_event_key), '') is null then
        raise exception 'heartbeat action token, actor, and provider event are required'
            using errcode = '22023';
    end if;
    select t.*, d.run_id, r.profile_id
      into token_row
      from public.heartbeat_action_tokens t
      join public.heartbeat_deliveries d on d.delivery_id = t.delivery_id
      join public.heartbeat_runs r on r.run_id = d.run_id
     where t.token_hash = p_token_hash
     for update of t;
    if not found or token_row.consumed_at is not null
       or token_row.expires_at <= now() then
        raise exception 'heartbeat action token is invalid, expired, or already used'
            using errcode = '42501';
    end if;
    select exists (
        select 1 from public.heartbeat_profile_grants
        where profile_id = token_row.profile_id
          and permission in ('review', 'admin')
          and subject_kind = 'principal'
          and subject_ref = p_actor_ref
    ) into allowed;
    if not allowed then
        raise exception 'actor is not a heartbeat reviewer' using errcode = '42501';
    end if;
    select * into item_row from public.heartbeat_items
     where item_id = token_row.item_id for update;
    if not found or item_row.version <> token_row.item_version then
        raise exception 'heartbeat item changed after this action was rendered'
            using errcode = '40001';
    end if;
    to_status := item_row.status;
    disposition_value := item_row.disposition;
    snooze_value := item_row.snooze_until;
    if token_row.action in ('approve', 'assign', 'park') then
        to_status := 'resolved';
        disposition_value := token_row.action;
    elsif token_row.action = 'snooze' then
        snooze_value := coalesce((token_row.payload->>'until')::timestamptz, now() + interval '1 day');
        to_status := 'snoozed';
        disposition_value := 'snooze';
    elsif token_row.action = 'not_useful' then
        to_status := 'dismissed';
        disposition_value := 'not_useful';
    elsif token_row.action <> 'prepare_draft' then
        raise exception 'unsupported heartbeat action %', token_row.action using errcode = '22023';
    end if;
    new_version := item_row.version + 1;
    update public.heartbeat_items set status = to_status, disposition = disposition_value,
        snooze_until = snooze_value,
        resolved_at = case when to_status in ('resolved', 'dismissed') then now() else null end,
        version = new_version
      where item_id = item_row.item_id;
    update public.heartbeat_action_tokens set consumed_at = now(), consumed_by_principal = p_actor_ref
     where token_hash = p_token_hash;
    event_key := 'slack:' || p_provider_event_key;
    insert into public.heartbeat_item_events (
        event_id, item_id, run_id, event_type, from_status, to_status,
        item_version, actor_kind, actor_ref, payload, idempotency_key
    ) values (
        gen_random_uuid(), item_row.item_id, token_row.run_id, token_row.action,
        item_row.status, to_status, new_version, 'human', p_actor_ref,
        jsonb_build_object('delivery_id', token_row.delivery_id), event_key
    );
    return jsonb_build_object(
        'item_id', item_row.item_id,
        'action', token_row.action,
        'status', to_status,
        'version', new_version
    );
end;
$$;

alter function heartbeat_consume_action(text, text, text)
    owner to centaur_heartbeat_definer;

revoke all on function heartbeat_consume_action(text, text, text) from public;
grant execute on function heartbeat_consume_action(text, text, text)
    to centaur_heartbeat_feedback;
