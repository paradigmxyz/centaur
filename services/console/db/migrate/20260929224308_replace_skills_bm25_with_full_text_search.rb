class ReplaceSkillsBm25WithFullTextSearch < ActiveRecord::Migration[8.1]
  SEARCH_VECTOR = <<~SQL.squish
    setweight(to_tsvector('english', name), 'A') ||
    setweight(to_tsvector('english', description), 'B') ||
    setweight(to_tsvector('english', content), 'C')
  SQL

  def up
    # Skill search now uses built-in PostgreSQL full-text search so Console
    # runs on managed PostgreSQL services that do not offer pg_search.
    remove_index :skills, name: :index_skills_on_search_document, if_exists: true
    disable_extension "pg_search" if extension_enabled?("pg_search")

    add_column :skills, :search_vector, :virtual, type: :tsvector, as: SEARCH_VECTOR, stored: true
    add_index :skills, :search_vector, using: :gin
  end

  def down
    remove_index :skills, :search_vector
    remove_column :skills, :search_vector

    enable_extension "pg_search"
    execute <<~SQL.squish
      CREATE INDEX index_skills_on_search_document ON skills
      USING bm25 (id, name, description, content) WITH (key_field = 'id')
    SQL
  end
end
