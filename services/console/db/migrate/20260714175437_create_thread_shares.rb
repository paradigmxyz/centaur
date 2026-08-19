class CreateThreadShares < ActiveRecord::Migration[8.1]
  def change
    create_table :thread_shares do |t|
      t.string :thread_key, null: false, limit: 512
      t.references :created_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
    end

    add_index :thread_shares, :thread_key, unique: true
  end
end
