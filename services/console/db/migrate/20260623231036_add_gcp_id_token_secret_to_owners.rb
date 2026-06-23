class AddGcpIdTokenSecretToOwners < ActiveRecord::Migration[8.1]
  def change
    add_reference :secret_sources, :gcp_id_token_secret, null: true, foreign_key: true, index: { unique: true }
    add_reference :request_rules, :gcp_id_token_secret, null: true, foreign_key: true
    add_reference :grants, :gcp_id_token_secret, null: true, foreign_key: true
  end
end
