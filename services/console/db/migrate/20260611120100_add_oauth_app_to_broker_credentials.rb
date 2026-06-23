class AddOauthAppToBrokerCredentials < ActiveRecord::Migration[8.1]
  def change
    add_reference :broker_credentials, :oauth_app, null: true, foreign_key: true
    add_column :broker_credentials, :provider_subject, :string    # IdP-stable account id (Google `sub`)
    add_column :broker_credentials, :provider_email, :string      # account email at consent time
    add_column :broker_credentials, :external_user_key, :string   # opaque `user` key from the start URL

    # Upsert key for the consent flow: one credential per (app, provider account).
    add_index :broker_credentials, [ :oauth_app_id, :provider_subject ],
              unique: true, where: "provider_subject IS NOT NULL"

    # Flow-created credentials have no console operator behind them.
    change_column_null :broker_credentials, :created_by_id, true
  end
end
