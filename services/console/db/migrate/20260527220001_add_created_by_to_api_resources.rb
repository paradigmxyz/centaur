class AddCreatedByToApiResources < ActiveRecord::Migration[8.1]
  TABLES = %i[grants principals static_secrets].freeze

  def change
    TABLES.each do |table|
      add_reference table, :created_by, null: false, foreign_key: { to_table: :users }
    end
  end
end
