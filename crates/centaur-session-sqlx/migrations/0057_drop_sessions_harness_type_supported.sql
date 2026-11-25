-- api-rs validates harness_type through HarnessType on every write and read,
-- so adding a harness no longer requires a schema change.
alter table sessions
drop constraint sessions_harness_type_supported;
