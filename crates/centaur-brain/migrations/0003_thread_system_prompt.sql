-- The custom system prompt each thread runs with, pinned by its first turn
-- (empty for none) and never changed: a Console edit applies to new threads.
-- NULL means no turn has pinned one yet. Threads from before this column ran
-- without a custom prompt, so they keep none. The brain Deployment is
-- recreated, not rolled, so no old brain adds rows after the backfill.
alter table nanocode.thread_sessions add column if not exists system_prompt text;

update nanocode.thread_sessions set system_prompt = '' where system_prompt is null;

grant update (system_prompt) on nanocode.thread_sessions to centaur_brain;
