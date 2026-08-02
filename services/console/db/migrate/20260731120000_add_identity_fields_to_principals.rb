class AddIdentityFieldsToPrincipals < ActiveRecord::Migration[8.1]
  def up
    add_column :principals, :kind, :string, null: false, default: "unknown"
    add_column :principals, :slack_user_id, :string
    add_column :principals, :slack_channel_id, :string
    add_column :principals, :slack_team_id, :string
    add_column :principals, :slack_email, :string

    add_index :principals, [ :namespace, :kind ]
    add_index :principals, [ :namespace, :slack_user_id ]
    add_index :principals, [ :namespace, :slack_channel_id ]
    add_index :principals, [ :namespace, :slack_team_id ]
    add_index :principals, [ :namespace, :slack_email ]

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
          END,
          slack_user_id = NULLIF(TRIM(labels ->> 'slack_user_id'), ''),
          slack_channel_id = NULLIF(TRIM(labels ->> 'slack_channel_id'), ''),
          slack_team_id = NULLIF(TRIM(labels ->> 'slack_team_id'), ''),
          slack_email = NULLIF(TRIM(labels ->> 'slack_email'), '')
    SQL

    # Keep aliases synchronized for legacy readers during the cutover.
    execute <<~SQL.squish
      UPDATE principals
      SET labels = (labels - 'kind' - 'slack_user_id' - 'slack_channel_id' - 'slack_team_id' - 'slack_email') ||
                   CASE WHEN kind = 'unknown' THEN '{}'::jsonb
                        ELSE jsonb_build_object('kind', kind) END ||
                   CASE WHEN slack_user_id IS NULL THEN '{}'::jsonb
                        ELSE jsonb_build_object('slack_user_id', slack_user_id) END ||
                   CASE WHEN slack_channel_id IS NULL THEN '{}'::jsonb
                        ELSE jsonb_build_object('slack_channel_id', slack_channel_id) END ||
                   CASE WHEN slack_team_id IS NULL THEN '{}'::jsonb
                        ELSE jsonb_build_object('slack_team_id', slack_team_id) END ||
                   CASE WHEN slack_email IS NULL THEN '{}'::jsonb
                        ELSE jsonb_build_object('slack_email', slack_email) END
    SQL
  end

  def down
    remove_index :principals, [ :namespace, :slack_email ], if_exists: true
    remove_index :principals, [ :namespace, :slack_team_id ], if_exists: true
    remove_index :principals, [ :namespace, :slack_channel_id ], if_exists: true
    remove_index :principals, [ :namespace, :slack_user_id ], if_exists: true
    remove_index :principals, [ :namespace, :kind ], if_exists: true
    remove_column :principals, :slack_email, if_exists: true
    remove_column :principals, :slack_team_id, if_exists: true
    remove_column :principals, :slack_channel_id, if_exists: true
    remove_column :principals, :slack_user_id, if_exists: true
    remove_column :principals, :kind, if_exists: true
  end
end
