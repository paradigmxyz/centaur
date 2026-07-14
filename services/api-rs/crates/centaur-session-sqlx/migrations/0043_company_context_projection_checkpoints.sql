create table if not exists company_context_projection_checkpoints (
    scope text primary key,
    watermark timestamptz,
    window_start timestamptz,
    window_end timestamptz,
    cursor_updated_at timestamptz,
    cursor_key text not null default '',
    lease_token text,
    lease_expires_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (window_end is null or window_start is null or window_end >= window_start)
);

create index if not exists idx_company_context_projection_checkpoints_lease
    on company_context_projection_checkpoints (lease_expires_at)
    where lease_expires_at is not null;

do $$
declare
    role_name text;
begin
    foreach role_name in array array['centaur_slack_admin', 'centaur_readonly'] loop
        if exists (select 1 from pg_roles where rolname = role_name) then
            execute format(
                'grant select on company_context_projection_checkpoints to %I',
                role_name
            );
        end if;
    end loop;
end $$;

alter table company_context_projection_checkpoints enable row level security;

drop policy if exists centaur_readonly_company_context_projection_checkpoints_select
    on company_context_projection_checkpoints;
create policy centaur_readonly_company_context_projection_checkpoints_select
    on company_context_projection_checkpoints for select to centaur_readonly using (true);

do $$
begin
    if exists (select 1 from pg_roles where rolname = 'centaur_slack_admin') then
        drop policy if exists centaur_slack_admin_company_context_projection_checkpoints_select
            on company_context_projection_checkpoints;
        create policy centaur_slack_admin_company_context_projection_checkpoints_select
            on company_context_projection_checkpoints for select to centaur_slack_admin using (true);
    end if;
end $$;
