drop policy if exists centaur_cc_reader_channels_select
    on slack_sync_channels;
create policy centaur_cc_reader_channels_select
    on slack_sync_channels
    for select
    to centaur_company_context_reader
    using (
        channel_id = centaur_current_slack_channel_id()
        or channel_id = any(
            (select centaur_current_slack_history_channel_ids())::text[]
        )
        or (
            (select centaur_company_context_include_public_slack())
            and not is_private
        )
        or (
            is_private
            and centaur_can_read_slack_user_conversation(
                centaur_current_slack_team_id(),
                channel_id
            )
        )
    );
