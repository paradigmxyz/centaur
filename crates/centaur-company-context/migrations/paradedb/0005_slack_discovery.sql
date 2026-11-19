-- Slack conversations discovered through users' Slack broker credentials, and
-- the shared pacing state for the Slack app's Web API rate limits. Message
-- history and reader access are added once they are synchronized.

-- Slack limits each Web API method per workspace per app, so every token the
-- app issues in a workspace shares one budget per method. Workers reserve
-- send slots here so all replicas pace one shared schedule.
create table company_context_system.slack_rate_limits (
    app_slug text not null,
    team_id text not null,
    method text not null,
    -- Earliest time the next request may be sent.
    next_slot_at timestamptz not null default now(),
    -- Set from Retry-After when Slack rate limits the method.
    blocked_until timestamptz,
    -- Multiplier on the configured request interval, raised on rate limits
    -- and relaxed while none occur.
    backoff double precision not null default 1,
    backoff_adjusted_at timestamptz not null default now(),
    last_rate_limited_at timestamptz,
    updated_at timestamptz not null default now(),
    primary key (app_slug, team_id, method),
    check (app_slug <> ''),
    check (team_id <> ''),
    check (method <> ''),
    check (backoff >= 1)
);

create table company_context_system.slack_conversations (
    conversation_id text primary key,
    team_id text not null,
    kind text not null,
    name text not null default '',
    is_archived boolean not null default false,
    topic text not null default '',
    purpose text not null default '',
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (conversation_id <> ''),
    check (team_id <> ''),
    check (kind in ('public_channel', 'private_channel', 'mpim', 'im'))
);

-- The Slack identity behind each broker credential. Reader access will be
-- granted to identities with a live credential.
create table company_context_data.slack_broker_identities (
    broker_credential_id bigint primary key,
    team_id text not null,
    provider_subject text not null,
    active boolean not null default true,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (team_id <> ''),
    check (provider_subject <> '')
);

create table company_context_data.slack_broker_observations (
    broker_credential_id bigint not null,
    conversation_id text not null
        references company_context_system.slack_conversations(conversation_id) on delete cascade,
    provider_subject text not null,
    active boolean not null default true,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (broker_credential_id, conversation_id),
    check (provider_subject <> '')
);

create index slack_broker_observations_active_conversation_idx
    on company_context_data.slack_broker_observations (conversation_id)
    where active;
