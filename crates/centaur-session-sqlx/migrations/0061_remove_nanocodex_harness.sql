-- The nanocodex harness was removed, and api-rs rejects unknown harness types
-- when it reads a session. Move nanocodex sessions to Codex the way a harness
-- switch does: Codex cannot resume a Nanocodex thread, and the old sandbox
-- runs the removed harness, so detach it for the sandbox cleanup worker to
-- reap. Messages and events are kept.
update sessions
set harness_type = 'codex',
    harness_thread_id = null,
    sandbox_id = null,
    sandbox_repo_cache_enabled = null,
    sandbox_repo_cache_access = null,
    sandbox_observability_enabled = null,
    sandbox_last_active_at = null,
    updated_at = now()
where harness_type = 'nanocodex';
