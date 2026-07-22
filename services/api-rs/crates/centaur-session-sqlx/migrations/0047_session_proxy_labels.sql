alter table sessions
    add column if not exists proxy_labels jsonb not null default '{}'::jsonb;
