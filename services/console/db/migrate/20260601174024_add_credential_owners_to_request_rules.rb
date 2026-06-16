class AddCredentialOwnersToRequestRules < ActiveRecord::Migration[8.1]
  def change
    add_reference :request_rules, :gcp_auth_secret, null: true, foreign_key: true
    add_reference :request_rules, :oauth_token_secret, null: true, foreign_key: true
  end
end
