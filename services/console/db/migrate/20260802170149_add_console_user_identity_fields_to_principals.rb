class AddConsoleUserIdentityFieldsToPrincipals < ActiveRecord::Migration[8.1]
  class BackfillPrincipal < ActiveRecord::Base
    self.table_name = "principals"
  end

  class BackfillUser < ActiveRecord::Base
    self.table_name = "users"

    include OpaqueId
    oid_prefix "usr"
  end

  def up
    add_column :principals, :console_user_id, :bigint
    add_column :principals, :console_user_email, :string

    add_index :principals, [ :namespace, :console_user_id ]
    add_index :principals, [ :namespace, :console_user_email ]

    BackfillPrincipal.reset_column_information
    backfills = BackfillPrincipal.where(kind: "console_user").map do |principal|
      labels = principal.labels.to_h
      [ principal, labels, console_user_id_from_oid(labels["console-user-id"]) ]
    end
    backfills.each do |principal, labels, console_user_id|
      principal.update_columns(
        console_user_id: console_user_id,
        console_user_email: labels["email"],
        labels: ordinary_labels(labels)
      )
    end

    add_foreign_key :principals, :users, column: :console_user_id
  end

  def down
    remove_foreign_key :principals, column: :console_user_id, if_exists: true

    BackfillPrincipal.reset_column_information
    BackfillPrincipal.where(kind: "console_user").find_each do |principal|
      labels = principal.labels.to_h
      console_user_oid = console_user_oid_from_id(principal.console_user_id)
      labels["console-user-id"] = console_user_oid if console_user_oid
      labels["email"] = principal.console_user_email unless principal.console_user_email.nil?
      principal.update_columns(labels: labels)
    end

    remove_index :principals, [ :namespace, :console_user_email ], if_exists: true
    remove_index :principals, [ :namespace, :console_user_id ], if_exists: true
    remove_column :principals, :console_user_email, if_exists: true
    remove_column :principals, :console_user_id, if_exists: true
  end

  private

  def console_user_id_from_oid(oid)
    BackfillUser.find_by_oid(oid)&.id
  end

  def console_user_oid_from_id(id)
    BackfillUser.find_by(id: id)&.oid
  end

  def ordinary_labels(labels)
    labels.except("console-user-id", "email")
  end
end
