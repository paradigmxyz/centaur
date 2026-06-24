class AddPasswordGrantToBrokerCredentials < ActiveRecord::Migration[8.1]
  def change
    add_column :broker_credentials, :grant, :string, null: false, default: "refresh_token"
    add_column :broker_credentials, :username, :text
    add_column :broker_credentials, :password, :text
  end
end
