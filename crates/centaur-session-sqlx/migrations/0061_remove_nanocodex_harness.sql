-- The nanocodex harness was removed. api-rs rejects unknown harness types when
-- it reads a session, so move existing nanocodex sessions to Codex. Their
-- harness thread ids name Nanocodex runs, which Codex cannot resume.
update sessions
set harness_type = 'codex',
    harness_thread_id = null
where harness_type = 'nanocodex';
