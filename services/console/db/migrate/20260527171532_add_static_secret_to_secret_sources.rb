class AddStaticSecretToSecretSources < ActiveRecord::Migration[8.1]
  def change
    add_reference :secret_sources, :static_secret, null: true, foreign_key: true, index: { unique: true }
  end
end
