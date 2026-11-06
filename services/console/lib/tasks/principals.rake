namespace :principals do
  desc "Merge each Console user's Slack DM principal into their Console-user principal"
  task merge_slack_dm_principals: :environment do
    merged = 0
    Principal.where(kind: "console_user").where.not(console_user_id: nil).includes(:console_user).find_each do |principal|
      before = Principal.count
      ConsoleUserPrincipalProvisioner.call(principal.console_user)
      merged += before - Principal.count
    end
    puts "merged #{merged} Slack DM principal(s)"
  end
end
