class CreateSkillEditors < ActiveRecord::Migration[8.1]
  def change
    create_table :skill_editors do |t|
      t.references :skill, null: false, foreign_key: { on_delete: :cascade }
      t.references :user, null: false, foreign_key: { on_delete: :cascade }

      t.timestamps
    end

    add_index :skill_editors, [ :skill_id, :user_id ], unique: true
  end
end
