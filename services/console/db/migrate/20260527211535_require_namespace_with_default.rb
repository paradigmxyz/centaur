class RequireNamespaceWithDefault < ActiveRecord::Migration[8.1]
  def up
    Principal.where(namespace: nil).update_all(namespace: "default")
    StaticSecret.where(namespace: nil).update_all(namespace: "default")

    change_column_default :principals, :namespace, "default"
    change_column_null :principals, :namespace, false

    change_column_default :static_secrets, :namespace, "default"
    change_column_null :static_secrets, :namespace, false
  end

  def down
    change_column_null :static_secrets, :namespace, true
    change_column_default :static_secrets, :namespace, nil

    change_column_null :principals, :namespace, true
    change_column_default :principals, :namespace, nil
  end
end
