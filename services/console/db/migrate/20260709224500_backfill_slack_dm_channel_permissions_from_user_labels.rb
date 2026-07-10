class BackfillSlackDmChannelPermissionsFromUserLabels < ActiveRecord::Migration[8.1]
  def up
    catalog = SlackChannelCatalog.fetch
    return unless catalog.ok?

    dm_channel_ids_by_user_id = catalog.channels.each_with_object({}) do |channel, acc|
      next unless channel.id.start_with?("D")

      user_id = channel.name.to_s.strip.upcase
      acc[user_id] = channel.id if user_id.match?(/\A[UW][A-Z0-9]{2,}\z/)
    end
    return if dm_channel_ids_by_user_id.empty?

    principals_with_slack_user_ids.each do |principal_id, slack_user_id|
      channel_id = dm_channel_ids_by_user_id[slack_user_id]
      next if channel_id.blank?

      insert_dm_permission(principal_id, slack_user_id, channel_id)
    end
  end

  def down
    # One-way data backfill. Existing operators may edit these permissions after
    # migration, so rollback should not delete potentially modified rows.
  end

  private

  def principals_with_slack_user_ids
    connection.select_rows <<~SQL.squish
      SELECT id, upper(trim(labels->>'slack_user_id')) AS slack_user_id
      FROM principals
      WHERE upper(trim(labels->>'slack_user_id')) ~ '^[UW][A-Z0-9]{2,}$'
    SQL
  end

  def insert_dm_permission(principal_id, slack_user_id, channel_id)
    execute <<~SQL.squish
      INSERT INTO slack_channel_permissions (
        principal_id,
        channel_id,
        channel_name,
        upload_enabled,
        download_enabled,
        history_enabled,
        created_at,
        updated_at
      )
      VALUES (
        #{connection.quote(principal_id)},
        #{connection.quote(channel_id)},
        #{connection.quote(slack_user_id)},
        TRUE,
        TRUE,
        TRUE,
        CURRENT_TIMESTAMP,
        CURRENT_TIMESTAMP
      )
      ON CONFLICT (principal_id, channel_id) DO UPDATE
      SET
        channel_name = COALESCE(NULLIF(slack_channel_permissions.channel_name, ''), EXCLUDED.channel_name),
        updated_at = CURRENT_TIMESTAMP
    SQL
  end
end
