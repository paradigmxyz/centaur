-- Slack conversation members, so everyone in a conversation can read it, not
-- only the users whose credentials synced it. Each history sync replaces its
-- conversation's members with those Slack listed. Slack user IDs are scoped to
-- a workspace, so each member keeps the workspace of the credential that
-- listed it.
create table company_context_data.slack_conversation_members (
    conversation_id text not null
        references company_context_system.slack_conversations(conversation_id) on delete cascade,
    team_id text not null,
    user_id text not null,
    first_seen_at timestamptz not null default now(),
    primary key (conversation_id, user_id),
    check (team_id <> ''),
    check (user_id <> '')
);

create index slack_conversation_members_user_idx
    on company_context_data.slack_conversation_members (user_id, team_id);

-- The Slack users, by workspace and user ID, who can read each conversation:
-- those whose live credentials observe it, and its members while any live
-- credential does.
create view company_context_data.slack_conversation_readers as
select observations.conversation_id, identities.team_id, observations.provider_subject as user_id
from company_context_data.slack_broker_observations observations
join company_context_data.slack_broker_identities identities
  on identities.broker_credential_id = observations.broker_credential_id
where observations.active
union
select members.conversation_id, members.team_id, members.user_id
from company_context_data.slack_conversation_members members
where exists (
    select 1
    from company_context_data.slack_broker_observations observations
    where observations.conversation_id = members.conversation_id
      and observations.active
);
