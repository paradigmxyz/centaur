class AddIdentityFieldsToPrincipals < ActiveRecord::Migration[8.1]
  def up
    add_column :principals, :kind, :string, null: false, default: "unknown"

    add_index :principals, [ :namespace, :kind ]

    execute <<~SQL.squish
      UPDATE principals
      SET kind = CASE
            WHEN NULLIF(TRIM(labels ->> 'kind'), '') IS NOT NULL
              THEN TRIM(labels ->> 'kind')
            WHEN NULLIF(TRIM(labels ->> 'console-user-id'), '') IS NOT NULL OR
                 foreign_id LIKE 'console-user-%'
              THEN 'console_user'
            WHEN labels ->> 'purpose' IN ('warm-pool-bootstrap', 'workflow-host') OR
                 foreign_id IN ('warm-pool-bootstrap', 'workflow-host')
              THEN 'service'
            WHEN NULLIF(TRIM(labels ->> 'workflow_name'), '') IS NOT NULL OR
                 foreign_id LIKE 'workflow-%'
              THEN 'workflow'
            WHEN NULLIF(TRIM(labels ->> 'discord_guild_id'), '') IS NOT NULL OR
                 foreign_id LIKE 'discord-channel-%'
              THEN 'discord_channel'
            WHEN NULLIF(TRIM(labels ->> 'linear_issue_id'), '') IS NOT NULL OR
                 foreign_id LIKE 'linear-issue-%'
              THEN 'linear_issue'
            WHEN NULLIF(TRIM(labels ->> 'teams_user_id'), '') IS NOT NULL OR
                 foreign_id LIKE 'teams-user-%'
              THEN 'teams_user'
            WHEN NULLIF(TRIM(labels ->> 'teams_conversation_id'), '') IS NOT NULL OR
                 foreign_id LIKE 'teams-conversation-%'
              THEN 'teams_conversation'
            WHEN UPPER(TRIM(COALESCE(labels ->> 'slack_channel_id', ''))) LIKE 'D%'
              THEN 'slack_dm'
            WHEN NULLIF(TRIM(labels ->> 'slack_channel_id'), '') IS NOT NULL
              THEN 'slack_channel'
            WHEN foreign_id LIKE 'slack-user-%'
              THEN 'slack_dm'
            ELSE 'unknown'
          END
    SQL

    # Keep aliases synchronized for legacy readers during the cutover.
    execute <<~SQL.squish
      UPDATE principals
      SET labels = (labels - 'kind') ||
                   CASE WHEN kind = 'unknown' THEN '{}'::jsonb
                        ELSE jsonb_build_object('kind', kind) END
    SQL
  end

  def down
    remove_index :principals, [ :namespace, :kind ], if_exists: true
    remove_column :principals, :kind, if_exists: true
  end
end
