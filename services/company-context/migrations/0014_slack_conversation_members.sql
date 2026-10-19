-- Slack conversation members, so everyone in a conversation can read it, not
-- only the users whose credentials synced it. Each history sync replaces its
-- conversation's members with those Slack listed.
create table company_context_data.slack_conversation_members (
    conversation_id text not null
        references company_context_system.slack_conversations(conversation_id) on delete cascade,
    user_id text not null,
    first_seen_at timestamptz not null default now(),
    primary key (conversation_id, user_id),
    check (user_id <> '')
);

create index slack_conversation_members_user_idx
    on company_context_data.slack_conversation_members (user_id);

-- The Slack users who can read each conversation: those whose live
-- credentials observe it, and its members while any live credential does.
create view company_context_data.slack_conversation_readers as
select observations.conversation_id, observations.provider_subject as user_id
from company_context_data.slack_broker_observations observations
where observations.active
union
select members.conversation_id, members.user_id
from company_context_data.slack_conversation_members members
where exists (
    select 1
    from company_context_data.slack_broker_observations observations
    where observations.conversation_id = members.conversation_id
      and observations.active
);
