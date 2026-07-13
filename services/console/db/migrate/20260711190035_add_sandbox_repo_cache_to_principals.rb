class AddSandboxRepoCacheToPrincipals < ActiveRecord::Migration[8.1]
  LABEL_KEY = "centaur.sandbox_repo_cache"

  def up
    add_column :principals, :sandbox_repo_cache, :string

    execute <<~SQL.squish
      WITH normalized AS (
        SELECT id,
               CASE LOWER(TRIM(COALESCE(labels ->> '#{LABEL_KEY}', '')))
                 WHEN 'all' THEN 'all'
                 WHEN 'public' THEN 'public'
                 WHEN 'pub' THEN 'public'
                 WHEN 'none' THEN 'none'
                 ELSE CASE WHEN sandbox_repo_cache_enabled THEN 'all' ELSE 'none' END
               END AS repo_cache
        FROM principals
      )
      UPDATE principals
      SET sandbox_repo_cache = normalized.repo_cache,
          labels = (COALESCE(labels, '{}'::jsonb) - '#{LABEL_KEY}') ||
                   jsonb_build_object('#{LABEL_KEY}', normalized.repo_cache)
      FROM normalized
      WHERE principals.id = normalized.id
    SQL

    change_column_default :principals, :sandbox_repo_cache, from: nil, to: "all"
    change_column_null :principals, :sandbox_repo_cache, false
    remove_column :principals, :sandbox_repo_cache_enabled
  end

  def down
    add_column :principals, :sandbox_repo_cache_enabled, :boolean, null: false, default: true

    execute <<~SQL.squish
      UPDATE principals
      SET sandbox_repo_cache_enabled = (sandbox_repo_cache = 'all')
    SQL

    remove_column :principals, :sandbox_repo_cache
  end
end
