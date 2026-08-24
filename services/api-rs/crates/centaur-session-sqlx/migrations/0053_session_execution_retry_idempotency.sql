-- A failed or cancelled execution must not block a fresh execution that
-- carries the same idempotency key. The original index covered every row,
-- so once a turn died -- for example on a control-plane roll before the
-- adoption hardening landed -- the stable key of its trigger (a GitHub
-- review is keyed per commit) made the work permanently un-rerunnable:
-- the insert hit the index and the caller got the dead row back with
-- created = false.
--
-- The index now covers only rows whose outcome still matters for dedupe:
-- in-flight work (queued / running) and completed work. Failed and
-- cancelled rows fall out of the index, so the next insert with the same
-- key creates a fresh execution instead of resurrecting the dead one.
drop index if exists session_executions_thread_idempotency_idx;

create unique index if not exists session_executions_active_idempotency_idx
    on session_executions (thread_key, idempotency_key)
    where idempotency_key is not null
      and status in ('queued', 'running', 'completed');
