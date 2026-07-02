class CreateMcpOauthRefreshTokens < ActiveRecord::Migration[8.1]
  def change
    create_table :mcp_oauth_refresh_tokens do |t|
      t.references :mcp_oauth_client, null: false, foreign_key: true
      t.references :user, null: false, foreign_key: true
      t.references :principal, null: false, foreign_key: true
      t.string :token_hash, null: false
      t.string :resource, null: false
      t.jsonb :scopes, null: false, default: []
      t.datetime :expires_at, null: false
      t.datetime :revoked_at
      t.datetime :last_used_at

      t.timestamps
    end

    add_index :mcp_oauth_refresh_tokens, :token_hash, unique: true
    add_index :mcp_oauth_refresh_tokens, :expires_at
  end
end
