class CreateGrants < ActiveRecord::Migration[8.1]
  def change
    create_table :grants do |t|
      t.references :principal, null: false, foreign_key: true
      t.references :static_secret, null: false, foreign_key: true

      t.timestamps
    end
  end
end
