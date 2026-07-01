class AddApiKeyToBrokerCredentials < ActiveRecord::Migration[8.1]
  def change
    add_column :broker_credentials, :api_key, :text
  end
end
