-- Deployment contract: the migration/operator role must bind each fixed
-- heartbeat DSN role once with the immutable proxy principal foreign_id:
-- select public.heartbeat_bind_workflow_principal('centaur_heartbeat_run', '<run foreign_id>');
-- and likewise for centaur_heartbeat_feedback and centaur_heartbeat_prepare_action.
-- Workflow roles cannot create or alter these owner-controlled bindings.
create table if not exists heartbeat_memory_action_tokens (
    token_hash text primary key,
    delivery_id uuid not null references heartbeat_deliveries(delivery_id) on delete cascade,
    fact_id uuid not null references memory_facts(fact_id) on delete cascade,
    expected_revision integer not null,
    action text not null check (action in ('confirm', 'dispute', 'forget', 'correct')),
    payload jsonb not null default '{}'::jsonb,
    expires_at timestamptz not null,
    consumed_at timestamptz,
    consumed_by_principal text,
    provider_event_key text,
    result jsonb,
    created_at timestamptz not null default now()
);
alter table heartbeat_deliveries add column if not exists token_seed text;
create table if not exists heartbeat_run_memory_facts (
    run_id uuid not null references heartbeat_runs(run_id) on delete cascade,
    fact_id uuid not null references memory_facts(fact_id) on delete cascade,
    primary key (run_id, fact_id)
);
create unique index if not exists heartbeat_memory_action_tokens_target_idx
    on heartbeat_memory_action_tokens(delivery_id, fact_id, action);

create table if not exists memory_correction_requests (
    request_id uuid primary key,
    fact_id uuid not null references memory_facts(fact_id) on delete cascade,
    requested_by text not null,
    requested_canonical_text text,
    requested_value jsonb,
    reason text,
    created_at timestamptz not null default now()
);
alter table memory_fact_events drop constraint if exists memory_fact_events_type_check;
alter table memory_fact_events add constraint memory_fact_events_type_check check
    (event_type in ('proposed','confirmed','disputed','superseded','forgotten','expired','evidence_added','correction_requested'));

create table if not exists heartbeat_draft_grants (
    grant_hash text primary key,
    delivery_id uuid references heartbeat_deliveries(delivery_id) on delete cascade,
    item_id uuid not null references heartbeat_items(item_id) on delete cascade,
    item_version integer not null,
    profile_id uuid not null references heartbeat_profiles(profile_id) on delete cascade,
    reviewer_ref text not null,
    expires_at timestamptz not null,
    read_at timestamptz,
    consumed_at timestamptz,
    created_at timestamptz not null default now()
);

create table if not exists heartbeat_draft_artifacts (
    artifact_id uuid primary key,
    profile_id uuid not null references heartbeat_profiles(profile_id) on delete cascade,
    item_id uuid not null references heartbeat_items(item_id) on delete cascade,
    item_version integer not null,
    grant_hash text not null references heartbeat_draft_grants(grant_hash),
    content jsonb not null,
    content_hash text not null,
    created_at timestamptz not null default now(),
    unique (item_id, item_version)
);

alter table heartbeat_action_tokens add column if not exists provider_event_key text;
alter table heartbeat_action_tokens add column if not exists result jsonb;

drop policy if exists heartbeat_memory_action_tokens_scope on heartbeat_memory_action_tokens;
drop policy if exists heartbeat_run_memory_facts_scope on heartbeat_run_memory_facts;
drop policy if exists memory_correction_requests_scope on memory_correction_requests;
drop policy if exists heartbeat_draft_grants_scope on heartbeat_draft_grants;
drop policy if exists heartbeat_draft_artifacts_scope on heartbeat_draft_artifacts;

do $$ begin
    if not exists (select 1 from pg_roles where rolname = 'centaur_heartbeat_prepare_action') then
        create role centaur_heartbeat_prepare_action nologin;
    end if;
end $$;
revoke all on heartbeat_draft_grants, heartbeat_draft_artifacts from public;
revoke all on heartbeat_draft_grants, heartbeat_draft_artifacts from centaur_heartbeat_prepare_action;
grant usage on schema public to centaur_heartbeat_prepare_action;
grant select on heartbeat_draft_grants, heartbeat_draft_artifacts to centaur_heartbeat_definer;
grant insert, select on heartbeat_memory_action_tokens to centaur_heartbeat_run;
grant insert, select on heartbeat_run_memory_facts to centaur_heartbeat_run;
revoke all on heartbeat_memory_action_tokens from centaur_heartbeat_feedback;
grant select, insert, update, delete on heartbeat_memory_action_tokens to centaur_heartbeat_definer;
grant select on heartbeat_observations, heartbeat_item_observations to centaur_heartbeat_definer;

alter table heartbeat_memory_action_tokens enable row level security;
alter table heartbeat_memory_action_tokens force row level security;
alter table memory_correction_requests enable row level security;
alter table memory_correction_requests force row level security;
alter table heartbeat_draft_grants enable row level security;
alter table heartbeat_draft_grants force row level security;
alter table heartbeat_draft_artifacts enable row level security;
alter table heartbeat_draft_artifacts force row level security;
alter table heartbeat_run_memory_facts enable row level security;
alter table heartbeat_run_memory_facts force row level security;

create policy heartbeat_memory_action_tokens_scope on heartbeat_memory_action_tokens
    using (heartbeat_memory_fact_visible(fact_id))
    with check (heartbeat_memory_fact_visible(fact_id));
create policy heartbeat_run_memory_facts_scope on heartbeat_run_memory_facts
    using (heartbeat_run_visible(run_id) and heartbeat_memory_fact_visible(fact_id))
    with check (heartbeat_run_visible(run_id) and heartbeat_memory_fact_visible(fact_id));
create policy memory_correction_requests_scope on memory_correction_requests
    using (heartbeat_memory_fact_visible(fact_id))
    with check (heartbeat_memory_fact_visible(fact_id));
create policy heartbeat_draft_grants_scope on heartbeat_draft_grants
    using (heartbeat_profile_visible(profile_id))
    with check (heartbeat_profile_visible(profile_id));
create policy heartbeat_draft_artifacts_scope on heartbeat_draft_artifacts
    using (heartbeat_profile_visible(profile_id))
    with check (heartbeat_profile_visible(profile_id));

create or replace function heartbeat_consume_memory_action(
    p_token_hash text, p_actor_ref text, p_provider_event_key text,
    p_corrected_text text default null, p_corrected_value jsonb default null,
    p_reason text default null
) returns jsonb language plpgsql security definer
set search_path = pg_catalog, public
as $$
declare t record; f record; delivery_profile record; r jsonb; next_status text; event_type text; req_id uuid;
begin
    select * into t from public.heartbeat_memory_action_tokens where token_hash = p_token_hash for update;
    if not found then raise exception 'memory action token is invalid' using errcode='42501'; end if;
    if t.consumed_at is not null then
        if t.provider_event_key = p_provider_event_key and t.consumed_by_principal = p_actor_ref and t.result is not null then return t.result; end if;
        raise exception 'memory action token is already used' using errcode='42501';
    end if;
    if t.expires_at <= now() then raise exception 'memory action token is expired' using errcode='42501'; end if;
    select hp.* into delivery_profile
      from public.heartbeat_deliveries d
      join public.heartbeat_runs hr on hr.run_id = d.run_id
      join public.heartbeat_profiles hp on hp.profile_id = hr.profile_id
     where d.delivery_id = t.delivery_id;
    if not found then raise exception 'memory action delivery is invalid' using errcode='42501'; end if;
    select * into f from public.memory_facts where fact_id = t.fact_id for update;
    if not found or f.revision <> t.expected_revision then
        raise exception 'memory fact revision is stale' using errcode='40001';
    end if;
    if not exists (
        select 1 from public.heartbeat_profiles p
        join public.heartbeat_profile_grants g on g.profile_id=p.profile_id
        where p.profile_id = delivery_profile.profile_id
          and p.namespace=f.namespace and p.scope_kind=f.scope_kind and p.scope_ref=f.scope_ref
          and p.executor_principal_foreign_id=f.owner_principal
          and g.subject_kind='principal' and g.subject_ref=p_actor_ref
          and g.permission in ('review','admin')
    ) then raise exception 'actor is not a memory reviewer' using errcode='42501'; end if;
    if t.action='confirm' then
        if f.status not in ('proposed','disputed') then raise exception 'memory fact cannot be confirmed from its current status' using errcode='22023'; end if;
        next_status := 'confirmed';
    elsif t.action='dispute' then
        if f.status in ('forgotten','superseded') then raise exception 'memory fact cannot be disputed from its current status' using errcode='22023'; end if;
        next_status := 'disputed';
    elsif t.action='forget' then next_status := 'forgotten';
    else
        req_id:=gen_random_uuid();
        insert into public.memory_correction_requests(request_id,fact_id,requested_by,requested_canonical_text,requested_value,reason)
            values(req_id,f.fact_id,p_actor_ref,p_corrected_text,p_corrected_value,p_reason);
        r := jsonb_build_object('fact_id',f.fact_id,'action','correct','status','correction_requested','fact_status',f.status,'revision',f.revision,'requested',true);
        insert into public.memory_fact_events(event_id,fact_id,event_type,actor_ref,reason,payload,idempotency_key)
            values(gen_random_uuid(),f.fact_id,'correction_requested',p_actor_ref,p_reason,jsonb_build_object('request_id',req_id),p_token_hash)
            on conflict (idempotency_key) do nothing;
        update public.heartbeat_memory_action_tokens set consumed_at=now(),consumed_by_principal=p_actor_ref,
            provider_event_key=p_provider_event_key,result=r where token_hash=p_token_hash;
        return r;
    end if;
    if next_status='forgotten' then
        with recursive lineage(fact_id, supersedes_fact_id) as (
            select fact_id, supersedes_fact_id from public.memory_facts where fact_id=f.fact_id
            union
            select x.fact_id, x.supersedes_fact_id from public.memory_facts x join lineage l
              on x.supersedes_fact_id=l.fact_id or l.supersedes_fact_id=x.fact_id
             where x.owner_principal=f.owner_principal and x.namespace=f.namespace
               and x.scope_kind=f.scope_kind and x.scope_ref=f.scope_ref
        )
        update public.memory_facts set status='forgotten', revision=revision+1,
            canonical_text='[forgotten]', value='{}'::jsonb, updated_at=now()
          where fact_id in (select fact_id from lineage);
        delete from public.memory_fact_evidence where fact_id in (
            with recursive lineage(fact_id, supersedes_fact_id) as (
                select fact_id, supersedes_fact_id from public.memory_facts where fact_id=f.fact_id
                union
                select x.fact_id, x.supersedes_fact_id from public.memory_facts x join lineage l
                  on x.supersedes_fact_id=l.fact_id or l.supersedes_fact_id=x.fact_id
                 where x.owner_principal=f.owner_principal and x.namespace=f.namespace
                   and x.scope_kind=f.scope_kind and x.scope_ref=f.scope_ref
            ) select fact_id from lineage
        );
        insert into public.memory_fact_events(event_id,fact_id,event_type,actor_ref,payload,idempotency_key)
        select gen_random_uuid(), lineage.fact_id, 'forgotten', p_actor_ref,
               jsonb_build_object('expected_revision',t.expected_revision),
               p_token_hash || ':' || lineage.fact_id::text
          from (
            with recursive lineage(fact_id, supersedes_fact_id) as (
                select fact_id, supersedes_fact_id from public.memory_facts where fact_id=f.fact_id
                union
                select x.fact_id, x.supersedes_fact_id from public.memory_facts x join lineage l
                  on x.supersedes_fact_id=l.fact_id or l.supersedes_fact_id=x.fact_id
                 where x.owner_principal=f.owner_principal and x.namespace=f.namespace
                   and x.scope_kind=f.scope_kind and x.scope_ref=f.scope_ref
            ) select fact_id from lineage
          ) lineage
        on conflict (idempotency_key) do nothing;
    else
        update public.memory_facts set status=next_status, revision=revision+1,
            updated_at=now(), confirmed_by_principal=case when next_status='confirmed' then p_actor_ref else confirmed_by_principal end
          where fact_id=f.fact_id;
        event_type := case next_status when 'confirmed' then 'confirmed' when 'disputed' then 'disputed' else next_status end;
        insert into public.memory_fact_events(event_id,fact_id,event_type,actor_ref,payload,idempotency_key)
            values(gen_random_uuid(),f.fact_id,event_type,p_actor_ref,jsonb_build_object('expected_revision',t.expected_revision),p_token_hash)
            on conflict (idempotency_key) do nothing;
    end if;
    r := jsonb_build_object('fact_id',f.fact_id,'action',t.action,'status',next_status,'revision',f.revision+1);
    update public.heartbeat_memory_action_tokens set consumed_at=now(),consumed_by_principal=p_actor_ref,
        provider_event_key=p_provider_event_key,result=r where token_hash=p_token_hash;
    return r;
end;
$$;
alter function heartbeat_consume_memory_action(text,text,text,text,jsonb,text) owner to centaur_heartbeat_definer;
revoke all on function heartbeat_consume_memory_action(text,text,text,text,jsonb,text) from public;
grant execute on function heartbeat_consume_memory_action(text,text,text,text,jsonb,text) to centaur_heartbeat_feedback;

create or replace function heartbeat_get_item(p_grant_hash text)
returns jsonb language plpgsql security definer
set search_path = pg_catalog, public
as $$ declare g record; i record; r jsonb;
begin
 select * into g from public.heartbeat_draft_grants where grant_hash=p_grant_hash for update;
 if not found or g.expires_at<=now() or g.consumed_at is not null then raise exception 'draft grant is invalid' using errcode='42501'; end if;
 select * into i from public.heartbeat_items where item_id=g.item_id and profile_id=g.profile_id and version=g.item_version;
 if not found then raise exception 'draft grant item is stale' using errcode='40001'; end if;
 update public.heartbeat_draft_grants set read_at=coalesce(read_at,now()) where grant_hash=p_grant_hash;
 return to_jsonb(i) || jsonb_build_object('observations',coalesce((select jsonb_agg(to_jsonb(o)) from public.heartbeat_observations o join public.heartbeat_item_observations io on io.observation_id=o.observation_id where io.item_id=i.item_id),'[]'::jsonb));
end; $$;
alter function heartbeat_get_item(text) owner to centaur_heartbeat_definer;
revoke all on function heartbeat_get_item(text) from public;
grant execute on function heartbeat_get_item(text) to centaur_heartbeat_prepare_action;

drop function if exists heartbeat_put_draft_artifact(text,jsonb);
create or replace function heartbeat_put_draft_artifact(
    p_grant_hash text, p_item_id uuid, p_item_version integer, p_content jsonb
)
returns jsonb language plpgsql security definer
set search_path = pg_catalog, public
as $$ declare g record; aid uuid; r jsonb;
begin
 select * into g from public.heartbeat_draft_grants where grant_hash=p_grant_hash for update;
 if not found or g.expires_at<=now() or g.consumed_at is not null then raise exception 'draft grant is invalid or already used' using errcode='42501'; end if;
 if g.read_at is null then raise exception 'draft grant must be read before writing' using errcode='42501'; end if;
 if jsonb_typeof(p_content) <> 'object' then raise exception 'draft content must be a JSON object' using errcode='22023'; end if;
 if g.item_id <> p_item_id or g.item_version <> p_item_version then raise exception 'draft grant item is not authorized' using errcode='42501'; end if;
 if not exists (select 1 from public.heartbeat_items where item_id=p_item_id and profile_id=g.profile_id and version=p_item_version and status='open') then raise exception 'draft grant item is stale' using errcode='40001'; end if;
 aid:=gen_random_uuid();
 insert into public.heartbeat_draft_artifacts(artifact_id,profile_id,item_id,item_version,grant_hash,content,content_hash)
 values(aid,g.profile_id,g.item_id,g.item_version,p_grant_hash,p_content,encode(sha256(convert_to(p_content::text,'UTF8')),'hex'));
 update public.heartbeat_draft_grants set consumed_at=now() where grant_hash=p_grant_hash;
 r:=jsonb_build_object('artifact_id',aid,'item_id',g.item_id,'item_version',g.item_version);
 return r;
end; $$;
alter function heartbeat_put_draft_artifact(text,uuid,integer,jsonb) owner to centaur_heartbeat_definer;
revoke all on function heartbeat_put_draft_artifact(text,uuid,integer,jsonb) from public;
grant execute on function heartbeat_put_draft_artifact(text,uuid,integer,jsonb) to centaur_heartbeat_prepare_action;

grant select,insert,update,delete on heartbeat_draft_grants,heartbeat_draft_artifacts,
    memory_correction_requests to centaur_heartbeat_definer;
