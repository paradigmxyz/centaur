class AddConsoleUserIdentityFieldsToPrincipals < ActiveRecord::Migration[8.1]
  CONSOLE_USER_ID_SQL = "labels ->> 'console-user-id'".freeze
  CONSOLE_USER_EMAIL_SQL = "labels ->> 'email'".freeze
  ORDINARY_LABELS_SQL = "labels - 'console-user-id' - 'email'".freeze
  RESTORED_LABELS_SQL = <<~SQL.squish.freeze
    labels || jsonb_strip_nulls(jsonb_build_object(
      'console-user-id', console_user_id,
      'email', console_user_email
    ))
  SQL

  def up
    add_column :principals, :console_user_id, :string
    add_column :principals, :console_user_email, :string

    add_index :principals, [ :namespace, :console_user_id ]
    add_index :principals, [ :namespace, :console_user_email ]

    execute <<~SQL.squish
      UPDATE principals
      SET console_user_id = #{CONSOLE_USER_ID_SQL},
          console_user_email = #{CONSOLE_USER_EMAIL_SQL},
          labels = #{ORDINARY_LABELS_SQL}
      WHERE kind = 'console_user'
    SQL
  end

  def down
    execute <<~SQL.squish
      UPDATE principals
      SET labels = #{RESTORED_LABELS_SQL}
      WHERE kind = 'console_user'
    SQL

    remove_index :principals, [ :namespace, :console_user_email ], if_exists: true
    remove_index :principals, [ :namespace, :console_user_id ], if_exists: true
    remove_column :principals, :console_user_email, if_exists: true
    remove_column :principals, :console_user_id, if_exists: true
  end
end
