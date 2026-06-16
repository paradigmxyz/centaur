class CreateSecretSources < ActiveRecord::Migration[8.1]
  def change
    create_table :secret_sources do |t|
      t.string :source_type, null: false
      t.jsonb :config, null: false, default: {}

      t.timestamps
    end

    add_index :secret_sources, :source_type
  end
end
