# api-rs runs a user's scheduled tasks as their Console principal but cannot
# create it. Console now provisions it when a task is saved; provision it for
# the authors of tasks saved before then.
class ProvisionScheduledTaskAuthorPrincipals < ActiveRecord::Migration[8.1]
  def up
    author_ids = select_values("SELECT DISTINCT author_id FROM scheduled_tasks WHERE author_id IS NOT NULL")
    User.where(id: author_ids).find_each do |user|
      ConsoleUserPrincipalProvisioner.call(user)
    end
  end

  def down
    # Provisioned principals stay: Console reuses them for the user's sessions.
  end
end
