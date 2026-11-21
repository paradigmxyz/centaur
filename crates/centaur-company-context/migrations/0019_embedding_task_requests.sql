-- Slack channel days and Drive documents whose publication has not finished
-- are enqueued again once their last request is an hour old, as Slack files
-- already are, so an embedding task that exhausted its attempts, for example
-- while the embeddings API refused requests, cannot leave them unpublished.
-- Rows that predate this column have no request recorded and are enqueued at
-- the next opportunity.
alter table company_context_system.slack_channel_days
    add column task_requested_at timestamptz;

alter table company_context_system.google_drive_files
    add column task_requested_at timestamptz;

create index google_drive_files_unpublished_idx
    on company_context_system.google_drive_files (task_requested_at)
    where extraction_status = 'completed'
      and embedding_status in ('pending', 'failed');
