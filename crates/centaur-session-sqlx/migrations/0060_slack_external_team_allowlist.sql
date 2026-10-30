-- Slack Connect workspaces whose messages Slackbot accepts, managed from the
-- Console. A single row: its absence means the list was never configured.
create table slack_external_team_allowlist (
    singleton boolean primary key default true check (singleton),
    team_ids text[] not null default '{}',
    updated_at timestamptz not null default now()
);
