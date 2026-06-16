class PrunePrincipalSyncConfigSnapshotsJob < ApplicationJob
  queue_as :default

  def perform
    PrincipalSyncConfigSnapshot.prune_expired!
  end
end
