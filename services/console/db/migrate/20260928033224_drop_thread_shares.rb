class DropThreadShares < ActiveRecord::Migration[8.1]
  def change
    drop_table :thread_shares do |t|
      t.string :thread_key, null: false, limit: 512
      t.references :created_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
      t.index :thread_key, unique: true
    end
  end
end
