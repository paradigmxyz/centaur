class PrincipalSyncConfigSnapshotWarmJob < ApplicationJob
  queue_as :default

  self.enqueue_after_transaction_commit = true

  limits_concurrency to: 1,
                     key: ->(principal_id) { "principal_sync_config_snapshot_#{principal_id}" },
                     duration: 1.minute,
                     on_conflict: :discard

  def perform(principal_id)
    principal = Principal.find_by(id: principal_id)
    return unless principal

    PrincipalSyncConfigSnapshot.build_for(principal)
  end
end
