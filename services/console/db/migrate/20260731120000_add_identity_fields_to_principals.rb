class AddIdentityFieldsToPrincipals < ActiveRecord::Migration[8.1]
  KIND_FROM_FOREIGN_ID_SQL = <<~SQL.squish.freeze
    CASE
      WHEN foreign_id IN ('warm-pool-bootstrap', 'workflow-host')
        THEN 'service'
      WHEN foreign_id LIKE 'console-user-%'
        THEN 'console_user'
      WHEN foreign_id LIKE 'workflow-%'
        THEN 'workflow'
      WHEN foreign_id LIKE 'discord-channel-%'
        THEN 'discord_channel'
      WHEN foreign_id LIKE 'linear-issue-%'
        THEN 'linear_issue'
      WHEN foreign_id LIKE 'teams-user-%'
        THEN 'teams_user'
      WHEN foreign_id LIKE 'teams-conversation-%'
        THEN 'teams_conversation'
      WHEN foreign_id ~ '^slack-user-t[a-z0-9]+-u[a-z0-9]+$'
        THEN 'slack_dm'
      WHEN foreign_id ~ '^slack-channel-t[a-z0-9]+-d[a-z0-9]+$'
        THEN 'slack_dm'
      WHEN foreign_id ~ '^slack-channel-d[a-z0-9]+$'
        THEN 'unknown'
      WHEN foreign_id LIKE 'slack-channel-%'
        THEN 'slack_channel'
      ELSE 'unknown'
    END
  SQL
  ORDINARY_LABELS_SQL = <<~SQL.squish.freeze
    labels - 'kind' - 'slack_user_id' - 'slack_channel_id' - 'slack_team_id' - 'slack_email'
  SQL

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
      SET kind = #{KIND_FROM_FOREIGN_ID_SQL},
          slack_user_id = NULLIF(TRIM(labels ->> 'slack_user_id'), ''),
          slack_channel_id = NULLIF(TRIM(labels ->> 'slack_channel_id'), ''),
          slack_team_id = NULLIF(TRIM(labels ->> 'slack_team_id'), ''),
          slack_email = NULLIF(TRIM(labels ->> 'slack_email'), '')
    SQL

    # Identity aliases are accepted and synthesized at the API boundary during
    # the cutover, but columns are the only persisted identity representation.
    execute <<~SQL.squish
      UPDATE principals
      SET labels = #{ORDINARY_LABELS_SQL}
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
