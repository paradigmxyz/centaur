class AddFilesHostToSlackOauthCredentials < ActiveRecord::Migration[8.1]
  def up
    execute <<~SQL
      INSERT INTO request_rules (
        static_secret_id,
        host,
        http_methods,
        paths,
        position,
        created_at,
        updated_at
      )
      SELECT
        static_secrets.id,
        'files.slack.com',
        '[]'::jsonb,
        '[]'::jsonb,
        COALESCE(MAX(existing_rules.position), -1) + 1,
        CURRENT_TIMESTAMP,
        CURRENT_TIMESTAMP
      FROM static_secrets
      INNER JOIN broker_credentials
        ON broker_credentials.id = static_secrets.broker_credential_id
      INNER JOIN oauth_apps
        ON oauth_apps.id = broker_credentials.oauth_app_id
      LEFT JOIN request_rules AS existing_rules
        ON existing_rules.static_secret_id = static_secrets.id
      WHERE oauth_apps.provider = 'slack'
        AND NOT EXISTS (
          SELECT 1
          FROM request_rules
          WHERE request_rules.static_secret_id = static_secrets.id
            AND lower(request_rules.host) = 'files.slack.com'
        )
      GROUP BY static_secrets.id
    SQL
  end

  def down
    raise ActiveRecord::IrreversibleMigration,
          "cannot distinguish migrated Slack file-host rules from operator-managed rules"
  end
end
