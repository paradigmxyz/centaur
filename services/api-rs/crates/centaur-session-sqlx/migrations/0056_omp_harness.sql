-- Like the other harness migrations, this is a forward-only SQLx migration.
-- Operational rollback disables OMP selection and preserves session history.
-- A constraint rollback to 0051 is safe only when no sessions use 'omp';
-- replacing this constraint with the 0051 definition then fails atomically
-- if any OMP rows remain. Never rewrite OMP sessions as another harness.
alter table sessions
drop constraint sessions_harness_type_supported;

alter table sessions
add constraint sessions_harness_type_supported
check (harness_type in ('codex', 'amp', 'claudecode', 'nanocodex', 'hermes', 'omp'));
