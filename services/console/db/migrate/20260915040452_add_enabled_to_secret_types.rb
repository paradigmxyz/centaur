class AddEnabledToSecretTypes < ActiveRecord::Migration[8.1]
  TABLES = %i[
    aws_auth_secrets
    gcp_auth_secrets
    gcp_id_token_secrets
    hmac_secrets
    oauth_token_secrets
    pg_dsn_secrets
    static_secrets
  ].freeze

  def change
    TABLES.each do |table|
      add_column table, :enabled, :boolean, default: true, null: false
    end
  end
end
