class AddOrganizationInstructionsToSystemSettings < ActiveRecord::Migration[8.1]
  def change
    add_column :system_settings, :organization_instructions_draft, :text, null: false, default: ""
    add_reference :system_settings,
                  :published_organization_instruction_version,
                  foreign_key: { to_table: :organization_instruction_versions }
  end
end
