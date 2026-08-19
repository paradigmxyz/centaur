class CreateSkills < ActiveRecord::Migration[8.1]
  def change
    enable_extension "pg_search" unless extension_enabled?("pg_search")

    create_table :skills do |t|
      t.references :user, null: false, foreign_key: { on_delete: :cascade }
      t.string :name, null: false
      t.text :description, null: false
      t.text :content, null: false
      t.string :visibility, null: false, default: "shared"
      t.datetime :shared_at
      t.datetime :archived_at
      t.integer :lock_version, null: false, default: 0

      t.timestamps
    end

    add_index :skills, :name, unique: true,
              where: "archived_at IS NULL", name: "index_active_skills_on_name"
    add_index :skills, [ :visibility, :updated_at ],
              where: "archived_at IS NULL", name: "index_active_skills_for_catalog"

    reversible do |direction|
      direction.up do
        add_bm25_index :skills,
                       fields: { id: {}, name: {}, description: {}, content: {} },
                       key_field: :id,
                       name: :index_skills_on_search_document
      end

      direction.down do
        remove_bm25_index :skills, name: :index_skills_on_search_document, if_exists: true
      end
    end
  end
end
