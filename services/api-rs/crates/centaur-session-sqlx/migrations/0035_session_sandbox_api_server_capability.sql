alter table sessions
    add column if not exists sandbox_api_server_enabled boolean default true;

alter table sessions
    alter column sandbox_api_server_enabled set default true;

update sessions
set sandbox_api_server_enabled = true
where sandbox_observability_enabled is not null
  and sandbox_api_server_enabled is null;
