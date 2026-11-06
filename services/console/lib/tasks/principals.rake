namespace :principals do
  desc "Link every Console user with a verified Slack identity to the user principal Slack knows them by"
  task link_users: :environment do
    users = User.where(id: UserIdentity.slack.select(:user_id))
    users.find_each { |user| ConsoleUserPrincipalProvisioner.call(user) }
    puts "linked #{users.count} Console user(s)"
  end
end
