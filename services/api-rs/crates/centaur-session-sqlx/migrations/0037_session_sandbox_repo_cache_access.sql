alter table sessions
    add column if not exists sandbox_repo_cache_access text;

update sessions
set sandbox_repo_cache_access = case
    when sandbox_repo_cache_enabled then 'all'
    else 'none'
end
where sandbox_repo_cache_access is null
  and sandbox_repo_cache_enabled is not null;
