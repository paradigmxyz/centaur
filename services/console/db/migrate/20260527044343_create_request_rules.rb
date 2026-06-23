class CreateRequestRules < ActiveRecord::Migration[8.1]
  def change
    create_table :request_rules do |t|
      t.string  :host
      t.string  :cidr
      t.jsonb   :http_methods, null: false, default: []
      t.jsonb   :paths, null: false, default: []
      t.integer :position, null: false, default: 0

      t.timestamps
    end

    add_index :request_rules, :position
    add_index :request_rules, :host
  end
end
