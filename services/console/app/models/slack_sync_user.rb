class SlackSyncUser < CentaurSessionRecord
  self.table_name = "slack_sync_users"

  def readonly?
    true
  end
end
