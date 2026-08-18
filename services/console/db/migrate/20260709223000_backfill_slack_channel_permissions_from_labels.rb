class BackfillSlackChannelPermissionsFromLabels < ActiveRecord::Migration[8.1]
  def up
    execute <<~SQL.squish
      INSERT INTO slack_channel_permissions (
        principal_id,
        channel_id,
        upload_enabled,
        download_enabled,
        history_enabled,
        created_at,
        updated_at
      )
      SELECT
        principals.id,
        upper(trim(principals.labels->>'slack_channel_id')),
        TRUE,
        TRUE,
        TRUE,
        CURRENT_TIMESTAMP,
        CURRENT_TIMESTAMP
      FROM principals
      WHERE upper(trim(principals.labels->>'slack_channel_id')) ~ '^[CDG][A-Z0-9]{8,}$'
      ON CONFLICT (principal_id, channel_id) DO NOTHING
    SQL
  end

  def down
    # One-way data backfill. Existing operators may edit these permissions after
    # migration, so rollback should not delete potentially modified rows.
  end
end
