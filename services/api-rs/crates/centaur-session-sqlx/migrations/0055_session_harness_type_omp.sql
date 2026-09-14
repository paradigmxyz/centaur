-- Register the omp (oh-my-pi) harness. The hermes migration rebuilt the
-- constraint without it, so extend the supported set here in a new file.
alter table sessions
    drop constraint sessions_harness_type_supported;
alter table sessions
    add constraint sessions_harness_type_supported
    check (harness_type in ('codex', 'amp', 'claudecode', 'nanocodex', 'omp', 'hermes'));
