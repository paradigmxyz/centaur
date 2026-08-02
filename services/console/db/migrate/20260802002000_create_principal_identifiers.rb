class CreatePrincipalIdentifiers < ActiveRecord::Migration[8.1]
  def up
    create_table :principal_identifiers do |t|
      t.references :principal, null: false
      t.string :namespace, null: false
      t.string :scheme, null: false
      t.string :issuer, null: false, default: ""
      t.string :subject, null: false
      t.jsonb :metadata, null: false, default: {}

      t.timestamps
    end

    add_index :principals,
              %i[id namespace],
              unique: true,
              name: "index_principals_on_id_and_namespace"
    add_foreign_key :principal_identifiers,
                    :principals,
                    column: %i[principal_id namespace],
                    primary_key: %i[id namespace],
                    on_delete: :cascade

    # Compatibility can temporarily produce the same external identifier on
    # more than one principal (for example separate Slack-DM and Console-user
    # principals). Enforce uniqueness only within one principal until those
    # rows have been collapsed, while still indexing the eventual resolver key.
    add_index :principal_identifiers,
              %i[principal_id scheme issuer subject],
              unique: true,
              name: "index_principal_identifiers_unique_per_principal"
    add_index :principal_identifiers,
              %i[namespace scheme issuer subject],
              name: "index_principal_identifiers_on_lookup_key"

    backfill_identifier("console_user", "console-user-id", metadata_label: "email")
    backfill_identifier("slack_user", "slack_user_id", issuer_label: "slack_team_id",
                        metadata_label: "slack_email")
    backfill_identifier("slack_channel", "slack_channel_id", issuer_label: "slack_team_id")
    backfill_discord_identifiers
    backfill_identifier("linear_issue", "linear_issue_id")
    backfill_identifier("teams_user", "teams_user_id")
    backfill_identifier("teams_conversation", "teams_conversation_id")
    backfill_identifier("workflow", "workflow_name")
    backfill_identifier("google_user", "google_subject", metadata_label: "google_email")
  end

  def down
    drop_table :principal_identifiers
    remove_index :principals, name: "index_principals_on_id_and_namespace", if_exists: true
  end

  private

  def backfill_identifier(scheme, subject_label, issuer_label: nil, metadata_label: nil)
    issuer_sql = issuer_label ? "COALESCE(NULLIF(TRIM(labels ->> #{quote(issuer_label)}), ''), '')" : "''"
    metadata_sql = if metadata_label
      "jsonb_strip_nulls(jsonb_build_object('email', NULLIF(TRIM(labels ->> #{quote(metadata_label)}), '')))"
    else
      "'{}'::jsonb"
    end

    execute <<~SQL.squish
      INSERT INTO principal_identifiers
        (principal_id, namespace, scheme, issuer, subject, metadata, created_at, updated_at)
      SELECT id,
             namespace,
             #{quote(scheme)},
             #{issuer_sql},
             NULLIF(TRIM(labels ->> #{quote(subject_label)}), ''),
             #{metadata_sql},
             CURRENT_TIMESTAMP,
             CURRENT_TIMESTAMP
      FROM principals
      WHERE NULLIF(TRIM(labels ->> #{quote(subject_label)}), '') IS NOT NULL
      ON CONFLICT (principal_id, scheme, issuer, subject) DO NOTHING
    SQL
  end

  def backfill_discord_identifiers
    execute <<~SQL.squish
      INSERT INTO principal_identifiers
        (principal_id, namespace, scheme, issuer, subject, metadata, created_at, updated_at)
      SELECT id,
             namespace,
             'discord_channel',
             COALESCE(NULLIF(TRIM(labels ->> 'discord_guild_id'), ''), ''),
             NULLIF(TRIM(labels ->> 'discord_channel_id'), ''),
             '{}'::jsonb,
             CURRENT_TIMESTAMP,
             CURRENT_TIMESTAMP
      FROM principals
      WHERE NULLIF(TRIM(labels ->> 'discord_channel_id'), '') IS NOT NULL
      ON CONFLICT (principal_id, scheme, issuer, subject) DO NOTHING
    SQL

    execute <<~SQL.squish
      INSERT INTO principal_identifiers
        (principal_id, namespace, scheme, issuer, subject, metadata, created_at, updated_at)
      SELECT id,
             namespace,
             'discord_guild',
             '',
             NULLIF(TRIM(labels ->> 'discord_guild_id'), ''),
             '{}'::jsonb,
             CURRENT_TIMESTAMP,
             CURRENT_TIMESTAMP
      FROM principals
      WHERE NULLIF(TRIM(labels ->> 'discord_guild_id'), '') IS NOT NULL
        AND NULLIF(TRIM(labels ->> 'discord_channel_id'), '') IS NULL
      ON CONFLICT (principal_id, scheme, issuer, subject) DO NOTHING
    SQL
  end

  def quote(value)
    connection.quote(value)
  end
end
