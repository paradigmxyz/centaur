-- File shares gate which Slack file documents a reader can see, so they live
-- with the data they gate, like the broker observations. Retrieval reads only
-- company_context_data.
alter table company_context_system.slack_file_shares
    set schema company_context_data;
