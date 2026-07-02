alter table sessions
    add column if not exists sandbox_api_server_enabled boolean;
