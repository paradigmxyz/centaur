module SlackDm
  # Keep already-enqueued jobs from the previous recurring configuration
  # compatible during rollout.
  class PollSyncJob < SyncCredentialJob; end
end
