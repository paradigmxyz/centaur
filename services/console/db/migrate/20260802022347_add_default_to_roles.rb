class AddDefaultToRoles < ActiveRecord::Migration[8.1]
  def up
    add_column :roles, :assign_by_default, :boolean, null: false, default: false

    execute <<~SQL
      INSERT INTO roles (namespace, foreign_id, name, labels, assign_by_default, created_by_id, created_at, updated_at)
      SELECT
        'default',
        'infra',
        'Infra',
        '{"managed-by":"centaur"}'::jsonb,
        TRUE,
        users.id,
        CURRENT_TIMESTAMP,
        CURRENT_TIMESTAMP
      FROM users
      ORDER BY users.id
      LIMIT 1
      ON CONFLICT (namespace, foreign_id) DO UPDATE SET assign_by_default = TRUE
    SQL
  end

  def down
    remove_column :roles, :assign_by_default
  end
end
