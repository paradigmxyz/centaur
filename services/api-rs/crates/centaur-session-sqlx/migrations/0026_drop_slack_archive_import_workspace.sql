drop index if exists idx_slack_archive_imports_workspace;

alter table slack_archive_imports
    drop column if exists workspace_id;
