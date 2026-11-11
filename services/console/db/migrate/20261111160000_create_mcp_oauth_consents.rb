class CreateMcpOauthConsents < ActiveRecord::Migration[8.1]
  def change
    # Server-side half of an MCP consent ticket (Mcp::ConsentTicket): one row
    # per GET /mcp/oauth/authorize that reached the consent page. A decision
    # consumes the row with a single conditional UPDATE, so a ticket can be
    # decided once even if the session cookie is replayed or two decisions
    # race. Only a digest of the nonce is stored.
    create_table :mcp_oauth_consents do |t|
      t.string :nonce_digest, null: false, index: { unique: true }
      t.references :user, null: false, foreign_key: { on_delete: :cascade }
      t.datetime :expires_at, null: false, index: true
      t.datetime :consumed_at

      t.timestamps
    end
  end
end
