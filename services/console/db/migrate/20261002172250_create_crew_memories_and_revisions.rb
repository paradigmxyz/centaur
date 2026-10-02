class CreateCrewMemoriesAndRevisions < ActiveRecord::Migration[8.1]
  def change
    create_table :crew_memories do |t|
      t.references :crew_profile, null: false, foreign_key: true
      t.string :scope_key, null: false
      t.string :key, null: false
      t.text :content, null: false
      t.string :source, null: false, default: ""
      t.datetime :expires_at
      t.integer :lock_version, null: false, default: 0
      t.timestamps
    end
    add_index :crew_memories, [ :crew_profile_id, :scope_key, :key ], unique: true

    create_table :crew_revisions do |t|
      t.references :crew_profile, null: false, foreign_key: true
      t.integer :version, null: false
      t.string :source, null: false
      t.jsonb :configuration, null: false
      t.timestamps
    end
    add_index :crew_revisions, [ :crew_profile_id, :version ], unique: true
  end
end
