class CreateCrewProfiles < ActiveRecord::Migration[8.1]
  def change
    create_table :crew_profiles do |t|
      t.string :crew_id, null: false, index: { unique: true }
      t.string :app_id, null: false, index: { unique: true }
      t.string :team_id, null: false
      t.references :principal, null: false, foreign_key: true, index: { unique: true }
      t.text :system_prompt, null: false, default: ""
      t.jsonb :skills, null: false, default: []
      t.jsonb :default_models, null: false, default: {}
      t.integer :lock_version, null: false, default: 0

      t.timestamps
    end
  end
end
