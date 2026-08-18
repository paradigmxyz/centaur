class AddKindToStaticSecrets < ActiveRecord::Migration[8.1]
  def change
    add_column :static_secrets, :kind, :string, default: "custom", null: false
  end
end
