class CreateSshKeySecrets < ActiveRecord::Migration[8.1]
  def change
    # A private SSH key granted like any other secret. The key itself lives in
    # one owned secret source; public_key is kept for display and registration.
    # owner_id marks a personal key managed from Console's Integrations page.
    create_table :ssh_key_secrets do |t|
      t.string :foreign_id, index: { unique: true }
      t.string :name, null: false
      t.string :description
      t.jsonb :labels, null: false, default: {}
      t.text :public_key, null: false
      t.references :owner, foreign_key: { to_table: :users }, index: { unique: true }
      t.references :created_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
    end
    add_index :ssh_key_secrets, :labels, using: :gin

    add_reference :secret_sources, :ssh_key_secret, null: true, foreign_key: true, index: { unique: true }
    add_reference :grants, :ssh_key_secret, null: true, foreign_key: true

    add_index :grants, [ :principal_id, :ssh_key_secret_id ], unique: true,
              where: "principal_id IS NOT NULL AND ssh_key_secret_id IS NOT NULL",
              name: "index_grants_uniq_principal_ssh_key_secret"
    add_index :grants, [ :role_id, :ssh_key_secret_id ], unique: true,
              where: "role_id IS NOT NULL AND ssh_key_secret_id IS NOT NULL",
              name: "index_grants_uniq_role_ssh_key_secret"
  end
end
