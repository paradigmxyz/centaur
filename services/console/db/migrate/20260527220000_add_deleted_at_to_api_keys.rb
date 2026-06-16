class AddDeletedAtToApiKeys < ActiveRecord::Migration[8.1]
  def change
    add_column :api_keys, :deleted_at, :datetime
    add_index :api_keys, :deleted_at
  end
end
