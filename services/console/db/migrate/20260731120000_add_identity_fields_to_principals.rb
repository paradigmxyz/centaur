class AddIdentityFieldsToPrincipals < ActiveRecord::Migration[8.1]
  KNOWN_KINDS = %w[
    unknown user console_user workflow slack_channel slack_dm discord_channel linear_issue
    teams_user teams_conversation
  ].freeze
  LEGACY_KIND_SQL = <<~SQL.squish.freeze
    CASE
      WHEN labels ->> 'kind' IN (#{KNOWN_KINDS.map { |kind| "'#{kind}'" }.join(", ")})
        THEN labels ->> 'kind'
      ELSE 'unknown'
    END
  SQL
  KIND_FROM_FOREIGN_ID_SQL = <<~SQL.squish.freeze
    CASE
      WHEN foreign_id LIKE 'console-user-%'
        THEN 'console_user'
      WHEN foreign_id = 'workflow-host'
        THEN 'unknown'
      WHEN foreign_id LIKE 'workflow-%'
        THEN 'workflow'
      WHEN foreign_id ~ '^slack-user-[te][a-z0-9]+-u[a-z0-9]+$'
        THEN 'slack_dm'
      WHEN foreign_id LIKE 'slack-user-%'
        THEN 'unknown'
      WHEN foreign_id ~ '^slack-channel-[te][a-z0-9]+-d[a-z0-9]+$'
        THEN 'slack_dm'
      WHEN foreign_id ~ '^slack-channel-d[a-z0-9]+$'
        THEN 'unknown'
      WHEN foreign_id LIKE 'slack-channel-%'
        THEN 'slack_channel'
      ELSE #{LEGACY_KIND_SQL}
    END
  SQL
  ORDINARY_LABELS_SQL = <<~SQL.squish.freeze
    labels - 'kind' - 'slack_user_id' - 'slack_channel_id' - 'slack_team_id' - 'slack_email'
  SQL
  SLACK_USER_ID_SQL = "labels ->> 'slack_user_id'".freeze
  SLACK_CHANNEL_ID_SQL = "labels ->> 'slack_channel_id'".freeze
  SLACK_TEAM_ID_SQL = "labels ->> 'slack_team_id'".freeze
  SLACK_EMAIL_SQL = "labels ->> 'slack_email'".freeze
  RESTORED_LABELS_SQL = <<~SQL.squish.freeze
    labels || jsonb_strip_nulls(jsonb_build_object(
      'kind', kind,
      'slack_user_id', slack_user_id,
      'slack_channel_id', slack_channel_id,
      'slack_team_id', slack_team_id,
      'slack_email', slack_email
    ))
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
          slack_user_id = #{SLACK_USER_ID_SQL},
          slack_channel_id = #{SLACK_CHANNEL_ID_SQL},
          slack_team_id = #{SLACK_TEAM_ID_SQL},
          slack_email = #{SLACK_EMAIL_SQL}
    SQL

    # Identity aliases are accepted and synthesized at the API boundary during
    # the cutover, but columns are the only persisted identity representation.
    execute <<~SQL.squish
      UPDATE principals
      SET labels = #{ORDINARY_LABELS_SQL}
    SQL
  end

  def down
    execute <<~SQL.squish
      UPDATE principals
      SET labels = #{RESTORED_LABELS_SQL}
    SQL

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
