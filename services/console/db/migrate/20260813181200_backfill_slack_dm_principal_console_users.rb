class BackfillSlackDmPrincipalConsoleUsers < ActiveRecord::Migration[8.1]
  def up
    execute <<~SQL
      UPDATE principals
      SET console_user_id = users.id,
          sync_config_cache_version = principals.sync_config_cache_version + 1,
          updated_at = CURRENT_TIMESTAMP
      FROM users
      WHERE principals.kind = 'slack_dm'
        AND principals.console_user_id IS NULL
        AND NULLIF(BTRIM(principals.slack_email), '') IS NOT NULL
        AND LOWER(BTRIM(principals.slack_email)) = LOWER(BTRIM(users.email))
    SQL
  end

  def down; end
end
