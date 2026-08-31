create table if not exists memory_facts (
    fact_id uuid primary key,
    namespace text not null,
    scope_kind text not null,
    scope_ref text not null,
    subject_key text not null,
    predicate text not null,
    value jsonb not null,
    canonical_text text not null,
    status text not null,
    sensitivity text not null default 'internal',
    confidence numeric(4, 3),
    valid_from timestamptz,
    valid_until timestamptz,
    observed_at timestamptz,
    revision integer not null default 1,
    supersedes_fact_id uuid references memory_facts(fact_id),
    promoted_from_fact_id uuid references memory_facts(fact_id),
    proposed_by_principal text,
    confirmed_by_principal text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint memory_facts_scope_kind_check
        check (scope_kind in ('organization', 'team', 'personal')),
    constraint memory_facts_status_check
        check (status in ('proposed', 'confirmed', 'disputed', 'superseded', 'forgotten', 'expired')),
    constraint memory_facts_sensitivity_check
        check (sensitivity in ('public', 'internal', 'confidential', 'restricted')),
    constraint memory_facts_confidence_check
        check (confidence is null or (confidence >= 0 and confidence <= 1)),
    constraint memory_facts_revision_check
        check (revision > 0)
);

create index if not exists memory_facts_scope_status_idx
    on memory_facts (namespace, scope_kind, scope_ref, status, updated_at desc);

create index if not exists memory_facts_subject_predicate_idx
    on memory_facts (namespace, scope_kind, scope_ref, subject_key, predicate, revision desc);

create table if not exists memory_fact_evidence (
    evidence_id uuid primary key,
    fact_id uuid not null references memory_facts(fact_id) on delete cascade,
    evidence_kind text not null,
    evidence_ref text not null,
    source_url text,
    excerpt text,
    content_hash text,
    created_at timestamptz not null default now(),
    constraint memory_fact_evidence_kind_check
        check (evidence_kind in ('heartbeat_observation', 'source_ref', 'user_statement', 'decision_record')),
    unique (fact_id, evidence_kind, evidence_ref)
);

create table if not exists memory_fact_events (
    event_id uuid primary key,
    fact_id uuid not null references memory_facts(fact_id) on delete cascade,
    event_type text not null,
    actor_ref text,
    reason text,
    payload jsonb not null default '{}'::jsonb,
    idempotency_key text not null unique,
    created_at timestamptz not null default now(),
    constraint memory_fact_events_type_check
        check (event_type in ('proposed', 'confirmed', 'disputed', 'superseded',
                              'forgotten', 'expired', 'evidence_added'))
);

create index if not exists memory_fact_events_fact_created_idx
    on memory_fact_events (fact_id, created_at, event_id);

revoke all on memory_facts, memory_fact_evidence, memory_fact_events from public;
