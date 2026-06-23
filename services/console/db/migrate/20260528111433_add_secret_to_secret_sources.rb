class AddSecretToSecretSources < ActiveRecord::Migration[8.1]
  def change
    add_column :secret_sources, :secret, :text
  end
end
