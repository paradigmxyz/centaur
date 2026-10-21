alter table sessions
    add column if not exists sandbox_repo_cache_enabled boolean,
    add column if not exists sandbox_observability_enabled boolean;
