class AddStaticSecretToRequestRules < ActiveRecord::Migration[8.1]
  def change
    add_reference :request_rules, :static_secret, null: true, foreign_key: true
  end
end
