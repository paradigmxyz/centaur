class CreateAwsAuthSecrets < ActiveRecord::Migration[8.1]
  def change
    create_table :aws_auth_secrets do |t|
      t.string :namespace, null: false, default: "default"
      t.string :foreign_id
      t.string :name
      t.string :description
      t.jsonb :labels, null: false, default: {}
      t.jsonb :allowed_regions, null: false, default: []
      t.jsonb :allowed_services, null: false, default: []
      t.references :created_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
    end

    add_index :aws_auth_secrets, [ :namespace, :foreign_id ], unique: true
    add_index :aws_auth_secrets, :labels, using: :gin
  end
end
