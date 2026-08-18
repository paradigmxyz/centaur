class AddTeamIdToUserIdentities < ActiveRecord::Migration[8.1]
  def change
    add_column :user_identities, :team_id, :string
  end
end
