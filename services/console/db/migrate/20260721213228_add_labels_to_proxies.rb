class AddLabelsToProxies < ActiveRecord::Migration[8.1]
  def change
    add_column :proxies, :labels, :jsonb, null: false, default: {}
    add_index :proxies, :labels, using: :gin
  end
end
