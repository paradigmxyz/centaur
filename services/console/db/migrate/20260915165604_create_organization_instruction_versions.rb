class CreateOrganizationInstructionVersions < ActiveRecord::Migration[8.1]
  def change
    create_table :organization_instruction_versions do |t|
      t.text :content, null: false
      t.references :published_by, null: false, foreign_key: { to_table: :users }

      t.timestamps
    end
  end
end
