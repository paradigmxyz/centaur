-- Complete the generic heartbeat semantic-memory pilot contract.  This
-- migration is additive: applied migrations remain immutable.

alter table public.heartbeat_profiles
    add column if not exists retention_policy jsonb not null default
        '{"observation_days":90,"run_snapshot_days":90,"delivery_days":180}'::jsonb;

alter table public.heartbeat_profiles
    drop constraint if exists heartbeat_profiles_retention_policy_check;
alter table public.heartbeat_profiles
    add constraint heartbeat_profiles_retention_policy_check check (
        jsonb_typeof(retention_policy) = 'object'
        and retention_policy - array['observation_days', 'run_snapshot_days', 'delivery_days'] = '{}'::jsonb
        and (retention_policy ?& array['observation_days', 'run_snapshot_days', 'delivery_days'])
        and jsonb_typeof(retention_policy->'observation_days') = 'number'
        and jsonb_typeof(retention_policy->'run_snapshot_days') = 'number'
        and jsonb_typeof(retention_policy->'delivery_days') = 'number'
        and (retention_policy->>'observation_days') ~ '^[1-9][0-9]{0,3}$'
        and (retention_policy->>'run_snapshot_days') ~ '^[1-9][0-9]{0,3}$'
        and (retention_policy->>'delivery_days') ~ '^[1-9][0-9]{0,3}$'
        and (retention_policy->>'observation_days')::integer between 1 and 3650
        and (retention_policy->>'run_snapshot_days')::integer between 1 and 3650
        and (retention_policy->>'delivery_days')::integer between 1 and 3650
    );

-- Promotion creates one immutable target fact for a source/target scope pair.
create unique index if not exists memory_facts_promotion_target_idx
    on public.memory_facts (promoted_from_fact_id, namespace, scope_kind, scope_ref)
    where promoted_from_fact_id is not null;

alter table public.memory_fact_events
    drop constraint if exists memory_fact_events_type_check;
alter table public.memory_fact_events
    add constraint memory_fact_events_type_check check
    (event_type in ('proposed','confirmed','disputed','superseded','forgotten',
                    'expired','evidence_added','correction_requested','promoted'));

-- A projection is derived from canonical memory_facts.  It has no evidence
-- excerpt, value, or source payload.  Only the definer writes this row; the
-- company-context reader policy is extended below with a narrow read branch.
create or replace function heartbeat_refresh_memory_projection()
returns trigger
language plpgsql
security definer
set search_path = pg_catalog, public
as $$
declare
    document_id text;
    canonical text;
begin
    if to_regclass('public.company_context_documents') is null then
        return coalesce(new, old);
    end if;

    execute $sql$
        delete from public.company_context_documents
         where source = 'heartbeat_memory'
           and source_type = 'memory_fact'
           and source_document_id = $1
    $sql$ using coalesce(new.fact_id, old.fact_id)::text;

    if tg_op = 'DELETE' then
        return old;
    end if;

    canonical := btrim(coalesce(new.canonical_text, ''));
    if new.scope_kind <> 'organization'
       or new.status <> 'confirmed'
       or new.sensitivity not in ('public', 'internal')
       or canonical = ''
       or length(canonical) > 1000
       or length(coalesce(new.namespace, '')) > 256
       or length(coalesce(new.scope_ref, '')) > 256
       or length(coalesce(new.subject_key, '')) > 256
       or length(coalesce(new.predicate, '')) > 256
       or (new.valid_from is not null and new.valid_from > now())
       or (new.valid_until is not null and new.valid_until <= now()) then
        return new;
    end if;

    document_id := format('memory_fact:%s:%s', new.fact_id, new.revision);
    execute $sql$
        insert into public.company_context_documents (
            document_id, source, source_type, source_document_id,
            source_chunk_id, title, body, url, access_scope,
            occurred_at, source_updated_at, content_hash, metadata
        ) values (
            $1, 'heartbeat_memory', 'memory_fact', $2, '', 'Confirmed memory',
            $3, '', format('organization:%s:%s', $7, $10), $4, $5,
            encode(sha256(convert_to($3, 'UTF8')), 'hex'),
            jsonb_build_object(
                'derived', true,
                'memory_fact_id', $2,
                'revision', $6,
                'namespace', $7,
                'scope_kind', $8,
                'scope_ref', $10,
                'sensitivity', $9,
                'subject_key', $11,
                'predicate', $12
            )
        )
        on conflict (document_id) do update set
            body = excluded.body,
            occurred_at = excluded.occurred_at,
            source_updated_at = excluded.source_updated_at,
            content_hash = excluded.content_hash,
            metadata = excluded.metadata,
            updated_at = now()
    $sql$ using
        document_id,
        new.fact_id::text,
        canonical,
        new.observed_at,
        new.updated_at,
        new.revision,
        new.namespace,
        new.scope_kind,
        new.sensitivity,
        new.scope_ref,
        new.subject_key,
        new.predicate;
    return new;
end;
$$;

-- Company-context consumers may read only deliverable organization memory.
-- Preserve the existing Slack branch exactly and do not expose team, private,
-- personal, unconfirmed, or non-derived rows through this reader role.
do $$
begin
    if to_regclass('public.company_context_documents') is not null
       and exists (select 1 from pg_roles where rolname = 'centaur_company_context_reader') then
        execute 'drop policy if exists centaur_cc_reader_documents_select on public.company_context_documents';
        if to_regclass('public.slack_sync_channels') is not null then
            execute $policy$
                create policy centaur_cc_reader_documents_select
                    on public.company_context_documents
                    for select
                    to centaur_company_context_reader
                    using (
                        (
                            source = 'slack'
                            and metadata ->> 'channel_id' in (
                                select channels.channel_id
                                from public.slack_sync_channels channels
                            )
                        )
                        or (
                            source = 'heartbeat_memory'
                            and source_type = 'memory_fact'
                            and access_scope like 'organization:%'
                            and metadata ->> 'scope_kind' = 'organization'
                            and metadata ->> 'sensitivity' in ('public', 'internal')
                            and metadata ->> 'derived' = 'true'
                            and nullif(current_setting('centaur.heartbeat_memory_namespace', true), '') = metadata ->> 'namespace'
                        )
                    )
            $policy$;
        else
            execute $policy$
                create policy centaur_cc_reader_documents_select
                    on public.company_context_documents
                    for select
                    to centaur_company_context_reader
                    using (
                        source = 'heartbeat_memory'
                        and source_type = 'memory_fact'
                        and access_scope like 'organization:%'
                        and metadata ->> 'scope_kind' = 'organization'
                        and metadata ->> 'sensitivity' in ('public', 'internal')
                        and metadata ->> 'derived' = 'true'
                        and nullif(current_setting('centaur.heartbeat_memory_namespace', true), '') = metadata ->> 'namespace'
                    )
            $policy$;
        end if;
    end if;
end
$$;

alter function heartbeat_refresh_memory_projection() owner to centaur_heartbeat_definer;
revoke all on function heartbeat_refresh_memory_projection() from public;
drop trigger if exists heartbeat_memory_projection on public.memory_facts;
create trigger heartbeat_memory_projection
    after insert or update or delete on public.memory_facts
    for each row execute function heartbeat_refresh_memory_projection();

do $$
begin
    if to_regclass('public.company_context_documents') is not null then
        execute 'grant select, insert, update, delete on public.company_context_documents to centaur_heartbeat_definer';
        execute 'drop policy if exists heartbeat_memory_projection_definer on public.company_context_documents';
        execute $policy$
            create policy heartbeat_memory_projection_definer
                on public.company_context_documents
                for all to centaur_heartbeat_definer
                using (source = 'heartbeat_memory' and source_type = 'memory_fact')
                with check (source = 'heartbeat_memory' and source_type = 'memory_fact')
        $policy$;
    end if;
end
$$;

grant select, update on public.heartbeat_observations,
    public.heartbeat_run_artifacts, public.heartbeat_runs,
    public.heartbeat_deliveries to centaur_heartbeat_definer;

-- The broad historical readonly policy predates heartbeat memory. Keep the
-- existing Slack/non-Slack behavior while explicitly excluding derived
-- heartbeat rows from ordinary readonly access.
do $$
begin
    if to_regclass('public.company_context_documents') is not null
       and exists (select 1 from pg_roles where rolname = 'centaur_readonly') then
        execute 'drop policy if exists centaur_readonly_company_context_documents_select on public.company_context_documents';
        if to_regclass('public.slack_sync_channels') is not null then
            execute $policy$
                create policy centaur_readonly_company_context_documents_select
                    on public.company_context_documents
                    for select to centaur_readonly
                    using (
                        source <> 'heartbeat_memory'
                        and (source <> 'slack' or exists (
                            select 1 from public.slack_sync_channels channels
                             where channels.channel_id = metadata ->> 'channel_id'
                        ))
                    )
            $policy$;
        else
            execute $policy$
                create policy centaur_readonly_company_context_documents_select
                    on public.company_context_documents
                    for select to centaur_readonly
                    using (source <> 'heartbeat_memory')
            $policy$;
        end if;
    end if;
end
$$;

-- Backfill already-confirmed organization facts when the generic context
-- table exists. This is intentionally a no-op in heartbeat-only test
-- databases that do not install the context subsystem.
update public.memory_facts
   set updated_at = updated_at
 where status = 'confirmed'
   and scope_kind = 'organization'
   and sensitivity in ('public', 'internal')
   and (valid_from is null or valid_from <= now())
   and (valid_until is null or valid_until > now());

-- Derived documents are never valid model evidence.  Keep this guard at the
-- database boundary as well as in the typed Python facade.
create or replace function heartbeat_memory_evidence_guard()
returns trigger
language plpgsql
security definer
set search_path = pg_catalog, public
as $$
begin
    if btrim(coalesce(new.evidence_ref, '')) ~* '^(memory_fact:|derived_memory:|memory-derived:)'
       or btrim(coalesce(new.source_url, '')) ~* '^(memory_fact:|derived_memory:|memory-derived:)'
    then
        raise exception 'derived memory cannot be used as evidence' using errcode = '22023';
    end if;
    return new;
end;
$$;
alter function heartbeat_memory_evidence_guard() owner to centaur_heartbeat_definer;
revoke all on function heartbeat_memory_evidence_guard() from public;
drop trigger if exists heartbeat_memory_evidence_guard on public.memory_fact_evidence;
create trigger heartbeat_memory_evidence_guard
    before insert or update on public.memory_fact_evidence
    for each row execute function heartbeat_memory_evidence_guard();

-- Feedback-only, reviewer/admin-authorized scope promotion.  This is a
-- server-side primitive; no model-selected target scope is wired into a
-- delivery button.
create or replace function heartbeat_deterministic_uuid(p_material text)
returns uuid
language sql
immutable
strict
set search_path = pg_catalog
as $$
    select encode(substr(sha256(convert_to($1, 'UTF8')), 1, 16), 'hex')::uuid
$$;
alter function heartbeat_deterministic_uuid(text) owner to centaur_heartbeat_definer;
revoke all on function heartbeat_deterministic_uuid(text) from public;

create or replace function heartbeat_promote_memory_fact(
    p_fact_id uuid,
    p_target_profile_id uuid,
    p_actor_ref text,
    p_expected_revision integer,
    p_idempotency_key text default null
) returns jsonb
language plpgsql
security definer
set search_path = pg_catalog, public
as $$
declare
    source_fact record;
    source_profile record;
    target_profile record;
    target_fact record;
    source_rank integer;
    target_rank integer;
    event_key text;
    target_id uuid;
begin
    if heartbeat_current_workflow_principal() <> 'workflow-heartbeat-feedback' then
        raise exception 'memory promotion requires the feedback workflow' using errcode = '42501';
    end if;
    if nullif(btrim(p_actor_ref), '') is null or p_expected_revision is null
       or p_expected_revision <= 0 then
        raise exception 'memory promotion actor and revision are required' using errcode = '22023';
    end if;

    select * into source_fact from public.memory_facts
     where fact_id = p_fact_id for update;
    if not found or source_fact.status <> 'confirmed'
       or source_fact.revision <> p_expected_revision then
        raise exception 'memory fact revision is stale or not confirmed' using errcode = '40001';
    end if;

    select p.* into source_profile
      from public.heartbeat_profiles p
      join public.heartbeat_profile_grants g on g.profile_id = p.profile_id
     where p.namespace = source_fact.namespace
       and p.scope_kind = source_fact.scope_kind
       and p.scope_ref = source_fact.scope_ref
       and p.executor_principal_foreign_id = source_fact.owner_principal
       and g.subject_kind = 'principal' and g.subject_ref = p_actor_ref
       and g.permission in ('review', 'admin')
     limit 1;
    if not found then
        raise exception 'actor is not a reviewer for the source memory scope' using errcode = '42501';
    end if;

    select p.* into target_profile from public.heartbeat_profiles p
     where p.profile_id = p_target_profile_id;
    if not found or target_profile.namespace <> source_fact.namespace
       or target_profile.workflow_name <> 'heartbeat_run' then
        raise exception 'memory promotion target is outside the source namespace' using errcode = '42501';
    end if;
    if target_profile.executor_principal_foreign_id is null then
        raise exception 'memory promotion target executor is invalid' using errcode = '42501';
    end if;
    if not exists (
        select 1 from public.heartbeat_profile_grants g
         where g.profile_id = target_profile.profile_id
           and g.subject_kind = 'principal' and g.subject_ref = p_actor_ref
           and g.permission = 'admin'
    ) then
        raise exception 'actor is not an administrator for the target memory scope' using errcode = '42501';
    end if;

    source_rank := case source_fact.scope_kind when 'personal' then 1 when 'team' then 2 when 'organization' then 3 else 0 end;
    target_rank := case target_profile.scope_kind when 'personal' then 1 when 'team' then 2 when 'organization' then 3 else 0 end;
    if target_rank <= source_rank then
        raise exception 'memory promotion target must be strictly broader' using errcode = '22023';
    end if;

    target_id := heartbeat_deterministic_uuid(
        format('ba55d079-050d-496d-a2a0-9c4f96e64c4f:memory-promotion:%s:%s',
               p_fact_id, p_target_profile_id));
    select * into target_fact from public.memory_facts
     where promoted_from_fact_id = p_fact_id
       and namespace = target_profile.namespace
       and scope_kind = target_profile.scope_kind
       and scope_ref = target_profile.scope_ref
     for update;
    if found then
        return jsonb_build_object('fact_id', target_fact.fact_id, 'source_fact_id', p_fact_id,
                                  'status', target_fact.status, 'revision', target_fact.revision,
                                  'promoted', true, 'replayed', true);
    end if;

    insert into public.memory_facts (
        fact_id, namespace, scope_kind, scope_ref, subject_key, predicate,
        value, canonical_text, status, sensitivity, confidence, valid_from,
        valid_until, observed_at, revision, promoted_from_fact_id,
        proposed_by_principal, confirmed_by_principal, owner_principal
    ) values (
        target_id, target_profile.namespace, target_profile.scope_kind,
        target_profile.scope_ref, source_fact.subject_key, source_fact.predicate,
        source_fact.value, source_fact.canonical_text, 'confirmed',
        source_fact.sensitivity, source_fact.confidence, source_fact.valid_from,
        source_fact.valid_until, source_fact.observed_at, 1, source_fact.fact_id,
        p_actor_ref, p_actor_ref, target_profile.executor_principal_foreign_id
    ) returning * into target_fact;

    insert into public.memory_fact_evidence (
        evidence_id, fact_id, evidence_kind, evidence_ref,
        source_url, excerpt, content_hash
    )
    select heartbeat_deterministic_uuid(
               format('ba55d079-050d-496d-a2a0-9c4f96e64f:memory-evidence:%s:%s:%s',
                      target_id, e.evidence_kind, e.evidence_ref)),
           target_id, e.evidence_kind, e.evidence_ref,
           e.source_url, e.excerpt, e.content_hash
      from public.memory_fact_evidence e
     where e.fact_id = p_fact_id
    on conflict (fact_id, evidence_kind, evidence_ref) do nothing;

    event_key := format(
        'memory-promotion:%s',
        encode(
            sha256(convert_to(
                format('%s:%s:%s', p_fact_id, p_target_profile_id,
                       coalesce(nullif(btrim(p_idempotency_key), ''), 'default')),
                'UTF8')),
            'hex'));
    insert into public.memory_fact_events
        (event_id, fact_id, event_type, actor_ref, payload, idempotency_key)
    values
        (heartbeat_deterministic_uuid(
             format('ba55d079-050d-496d-a2a0-9c4f96e64c4f:memory-event:%s:source', event_key)),
         p_fact_id, 'promoted', p_actor_ref,
         jsonb_build_object('promoted_fact_id', target_id, 'target_profile_id', p_target_profile_id,
                            'expected_revision', p_expected_revision), event_key)
    on conflict (idempotency_key) do nothing;
    insert into public.memory_fact_events
        (event_id, fact_id, event_type, actor_ref, payload, idempotency_key)
    values
        (heartbeat_deterministic_uuid(
             format('ba55d079-050d-496d-a2a0-9c4f96e64f:memory-event:%s:target', event_key)),
         target_id, 'promoted', p_actor_ref,
         jsonb_build_object('promoted_from_fact_id', p_fact_id, 'source_revision', p_expected_revision),
         event_key || ':target')
    on conflict (idempotency_key) do nothing;
    return jsonb_build_object('fact_id', target_id, 'source_fact_id', p_fact_id,
                              'status', 'confirmed', 'revision', 1,
                              'promoted', true, 'replayed', false);
exception
    when unique_violation then
        select * into target_fact from public.memory_facts
         where promoted_from_fact_id = p_fact_id
           and namespace = target_profile.namespace
           and scope_kind = target_profile.scope_kind
           and scope_ref = target_profile.scope_ref;
        if found then
            return jsonb_build_object('fact_id', target_fact.fact_id, 'source_fact_id', p_fact_id,
                                      'status', target_fact.status, 'revision', target_fact.revision,
                                      'promoted', true, 'replayed', true);
        end if;
        raise;
end;
$$;
alter function heartbeat_promote_memory_fact(uuid, uuid, text, integer, text)
    owner to centaur_heartbeat_definer;
revoke all on function heartbeat_promote_memory_fact(uuid, uuid, text, integer, text) from public;
grant execute on function heartbeat_promote_memory_fact(uuid, uuid, text, integer, text)
    to centaur_heartbeat_feedback;

-- Retention is an executor-only janitor.  It scrubs content but retains row
-- identities and immutable audit/event histories.
create or replace function heartbeat_apply_retention(p_profile_id uuid)
returns jsonb
language plpgsql
security definer
set search_path = pg_catalog, public
as $$
declare
    p record;
    observation_count integer := 0;
    artifact_count integer := 0;
    run_snapshot_count integer := 0;
    delivery_count integer := 0;
    action_token_count integer := 0;
    memory_token_count integer := 0;
    draft_artifact_count integer := 0;
    draft_grant_count integer := 0;
    expired_count integer := 0;
    affected integer := 0;
    fact_row record;
begin
    if heartbeat_current_workflow_principal() <> 'workflow-heartbeat-run' then
        raise exception 'retention requires the heartbeat run workflow' using errcode = '42501';
    end if;
    select * into p from public.heartbeat_profiles
     where profile_id = p_profile_id
       and executor_principal_foreign_id = heartbeat_current_workflow_principal();
    if not found then
        raise exception 'workflow principal does not operate this heartbeat profile' using errcode = '42501';
    end if;

    update public.heartbeat_observations
       set normalized_payload = '{}'::jsonb, title = '', source_url = null
     where profile_id = p_profile_id
       and captured_at < now() - make_interval(days => (p.retention_policy->>'observation_days')::integer)
       and (normalized_payload <> '{}'::jsonb or title <> '' or source_url is not null);
    get diagnostics observation_count = row_count;

    update public.heartbeat_run_artifacts a
       set content = '{}'::jsonb
      from public.heartbeat_runs r
     where r.run_id = a.run_id and r.profile_id = p_profile_id
       and r.started_at < now() - make_interval(days => (p.retention_policy->>'run_snapshot_days')::integer)
       and a.content <> '{}'::jsonb;
    get diagnostics artifact_count = row_count;

    update public.heartbeat_runs r
       set source_health = '{}'::jsonb, error = null
     where r.profile_id = p_profile_id
       and r.started_at < now() - make_interval(days => (p.retention_policy->>'run_snapshot_days')::integer)
       and (r.source_health <> '{}'::jsonb or r.error is not null);
    get diagnostics run_snapshot_count = row_count;

    update public.heartbeat_deliveries d
       set rendered_payload = '{}'::jsonb, error = null
      from public.heartbeat_runs r
     where r.run_id = d.run_id and r.profile_id = p_profile_id
       and d.created_at < now() - make_interval(days => (p.retention_policy->>'delivery_days')::integer)
       and (d.rendered_payload <> '{}'::jsonb or d.error is not null);
    get diagnostics delivery_count = row_count;

    delete from public.heartbeat_action_tokens t
     using public.heartbeat_deliveries d, public.heartbeat_runs r
     where t.delivery_id = d.delivery_id and d.run_id = r.run_id
       and r.profile_id = p_profile_id and (t.expires_at <= now()
         or t.created_at < now() - make_interval(days => (p.retention_policy->>'delivery_days')::integer));
    get diagnostics action_token_count = row_count;
    delete from public.heartbeat_memory_action_tokens t
     using public.heartbeat_deliveries d, public.heartbeat_runs r
     where t.delivery_id = d.delivery_id and d.run_id = r.run_id
       and r.profile_id = p_profile_id and t.expires_at <= now();
    get diagnostics affected = row_count;
    memory_token_count := affected;

    delete from public.heartbeat_draft_artifacts a
     using public.heartbeat_draft_grants g
     where a.grant_hash = g.grant_hash and a.profile_id = p_profile_id
       and (g.expires_at <= now() or a.created_at < now() - make_interval(days => (p.retention_policy->>'delivery_days')::integer));
    get diagnostics draft_artifact_count = row_count;
    delete from public.heartbeat_draft_grants g
     where g.profile_id = p_profile_id and g.expires_at <= now();
    get diagnostics affected = row_count;
    draft_grant_count := affected;

    for fact_row in
        select fact_id, revision from public.memory_facts
         where owner_principal = p.executor_principal_foreign_id
           and namespace = p.namespace and scope_kind = p.scope_kind and scope_ref = p.scope_ref
           and status in ('proposed', 'confirmed', 'disputed')
           and valid_until is not null and valid_until <= now()
         for update
    loop
        update public.memory_facts set status = 'expired', revision = revision + 1, updated_at = now()
         where fact_id = fact_row.fact_id;
        insert into public.memory_fact_events
            (event_id, fact_id, event_type, actor_ref, payload, idempotency_key)
        values
            (heartbeat_deterministic_uuid(
                 format('ba55d079-050d-496d-a2a0-9c4f96e64f:memory-event:retention:%s:%s',
                        fact_row.fact_id, fact_row.revision)),
             fact_row.fact_id, 'expired', p.executor_principal_foreign_id,
             jsonb_build_object('previous_revision', fact_row.revision),
             format('retention:%s:%s', fact_row.fact_id, fact_row.revision))
        on conflict (idempotency_key) do nothing;
        expired_count := expired_count + 1;
    end loop;

    -- Reconcile current projections, including facts whose valid_from has
    -- just arrived. The trigger removes stale revisions before re-inserting
    -- the bounded canonical projection.
    update public.memory_facts
       set updated_at = updated_at
     where owner_principal = p.executor_principal_foreign_id
       and namespace = p.namespace and scope_kind = p.scope_kind and scope_ref = p.scope_ref
       and status = 'confirmed'
       and (valid_from is null or valid_from <= now())
       and (valid_until is null or valid_until > now());

    return jsonb_build_object('observations_scrubbed', observation_count,
                              'artifacts_scrubbed', artifact_count,
                              'run_snapshots_scrubbed', run_snapshot_count,
                              'deliveries_scrubbed', delivery_count,
                              'action_tokens_deleted', action_token_count,
                              'memory_tokens_deleted', memory_token_count,
                              'draft_artifacts_deleted', draft_artifact_count,
                              'draft_grants_deleted', draft_grant_count,
                              'facts_expired', expired_count);
end;
$$;
alter function heartbeat_apply_retention(uuid) owner to centaur_heartbeat_definer;
revoke all on function heartbeat_apply_retention(uuid) from public;
grant execute on function heartbeat_apply_retention(uuid) to centaur_heartbeat_run;
