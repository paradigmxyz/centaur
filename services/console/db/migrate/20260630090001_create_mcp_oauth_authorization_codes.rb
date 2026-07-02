class CreateMcpOauthAuthorizationCodes < ActiveRecord::Migration[8.1]
  def change
    create_table :mcp_oauth_authorization_codes do |t|
      t.references :mcp_oauth_client, null: false, foreign_key: true
      t.references :user, null: false, foreign_key: true
      t.references :principal, null: false, foreign_key: true
      t.string :code_hash, null: false
      t.string :redirect_uri, null: false
      t.string :code_challenge, null: false
      t.string :resource, null: false
      t.jsonb :scopes, null: false, default: []
      t.datetime :expires_at, null: false
      t.datetime :consumed_at

      t.timestamps
    end

    add_index :mcp_oauth_authorization_codes, :code_hash, unique: true
    add_index :mcp_oauth_authorization_codes, :expires_at
  end
end
