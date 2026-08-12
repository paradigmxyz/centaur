class BackfillGithubTokenKind < ActiveRecord::Migration[8.1]
  def up
    execute <<~SQL.squish
      UPDATE static_secrets
        SET kind = 'github_token',
            inject_config = NULL,
            replace_config = jsonb_build_object(
              'proxy_value', 'GITHUB_TOKEN',
              'match_headers', jsonb_build_array('Authorization'),
              'require', FALSE
            ),
            updated_at = CURRENT_TIMESTAMP
        WHERE kind = 'custom'
          AND (
            (
              lower(COALESCE(inject_config->>'header', '')) = 'authorization'
              AND inject_config->>'formatter' = 'Bearer {{ .Value }}'
            )
            OR replace_config = jsonb_build_object(
              'proxy_value', 'GITHUB_TOKEN',
              'match_headers', jsonb_build_array('Authorization'),
              'require', FALSE
            )
          )
          AND EXISTS (
            SELECT 1 FROM request_rules
            WHERE request_rules.static_secret_id = static_secrets.id
              AND request_rules.host = 'api.github.com'
              AND request_rules.cidr IS NULL
              AND request_rules.http_methods = '[]'::jsonb
              AND request_rules.paths = '[]'::jsonb
              AND request_rules.position = 0
          )
          AND EXISTS (
            SELECT 1 FROM request_rules
            WHERE request_rules.static_secret_id = static_secrets.id
              AND request_rules.host = 'github.com'
              AND request_rules.cidr IS NULL
              AND request_rules.http_methods = '[]'::jsonb
              AND request_rules.paths = '[]'::jsonb
              AND request_rules.position = 1
          )
          AND 2 = (
            SELECT COUNT(*) FROM request_rules
            WHERE request_rules.static_secret_id = static_secrets.id
          )
    SQL
  end

  def down
    # One-way classification of the credential repaired by the preceding
    # migration. The schema migration removes kind on a full rollback.
  end
end
