class ReplaceGithubBearerInjection < ActiveRecord::Migration[8.1]
  def up
    execute <<~SQL.squish
      WITH updated_secrets AS (
        UPDATE static_secrets
        SET inject_config = NULL,
            replace_config = jsonb_build_object(
              'proxy_value', 'GITHUB_TOKEN',
              'match_headers', jsonb_build_array('Authorization'),
              'require', FALSE
            ),
            updated_at = CURRENT_TIMESTAMP
        WHERE lower(COALESCE(inject_config->>'header', '')) = 'authorization'
          AND inject_config->>'formatter' = 'Bearer {{ .Value }}'
          AND EXISTS (
            SELECT 1
            FROM request_rules
            WHERE request_rules.static_secret_id = static_secrets.id
              AND lower(request_rules.host) = 'github.com'
          )
        RETURNING id
      ), affected_principals AS (
        SELECT grants.principal_id AS id
        FROM grants
        INNER JOIN updated_secrets ON updated_secrets.id = grants.static_secret_id
        WHERE grants.principal_id IS NOT NULL
        UNION
        SELECT principal_roles.principal_id AS id
        FROM grants
        INNER JOIN updated_secrets ON updated_secrets.id = grants.static_secret_id
        INNER JOIN principal_roles ON principal_roles.role_id = grants.role_id
        WHERE grants.role_id IS NOT NULL
      )
      UPDATE principals
      SET sync_config_cache_version = sync_config_cache_version + 1,
          updated_at = CURRENT_TIMESTAMP
      WHERE principals.id IN (SELECT id FROM affected_principals)
    SQL
  end

  def down
    # One-way credential repair. Operators can edit these secrets after rollout,
    # so rollback must not restore a stale Bearer configuration over later work.
  end
end
