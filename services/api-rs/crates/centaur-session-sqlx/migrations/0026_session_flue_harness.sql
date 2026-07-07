alter table sessions
    drop constraint if exists sessions_harness_type_supported;

alter table sessions
    add constraint sessions_harness_type_supported
    check (harness_type in ('codex', 'amp', 'claudecode', 'flue'));
