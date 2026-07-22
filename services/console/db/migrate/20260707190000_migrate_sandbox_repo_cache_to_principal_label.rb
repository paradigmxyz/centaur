class MigrateSandboxRepoCacheToPrincipalLabel < ActiveRecord::Migration[8.1]
  LABEL_KEY = "centaur.sandbox_repo_cache"

  def up
    execute <<~SQL.squish
      UPDATE principals
      SET labels = COALESCE(labels, '{}'::jsonb) ||
                   jsonb_build_object(
                     '#{LABEL_KEY}',
                     CASE WHEN sandbox_repo_cache_enabled THEN 'all' ELSE 'none' END
                   ),
          sync_config_cache_version = COALESCE(sync_config_cache_version, 0) + 1,
          updated_at = NOW()
    SQL
  end

  def down
    execute <<~SQL.squish
      UPDATE principals
      SET sandbox_repo_cache_enabled = (labels ->> '#{LABEL_KEY}' = 'all'),
          labels = COALESCE(labels, '{}'::jsonb) - '#{LABEL_KEY}',
          sync_config_cache_version = COALESCE(sync_config_cache_version, 0) + 1,
          updated_at = NOW()
      WHERE labels ? '#{LABEL_KEY}'
    SQL
  end
end
