class CreateMcpOauthClients < ActiveRecord::Migration[8.1]
  def change
    create_table :mcp_oauth_clients do |t|
      t.string :name
      t.jsonb :redirect_uris, null: false, default: []
      t.jsonb :grant_types, null: false, default: []
      t.jsonb :response_types, null: false, default: []
      t.jsonb :scopes, null: false, default: []
      t.jsonb :metadata, null: false, default: {}
      t.datetime :last_used_at

      t.timestamps
    end
  end
end
