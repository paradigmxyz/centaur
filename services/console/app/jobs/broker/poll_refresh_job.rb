module Broker
  # The recurring driver of the refresh loop, replacing iron-token-broker's
  # per-credential goroutine. Level-triggered: every tick it re-derives which
  # credentials are due from the database, so a missed tick is caught by the
  # next one (unlike a self-rescheduling job, which orphans a credential forever
  # if its enqueue is ever lost).
  #
  # FOR UPDATE SKIP LOCKED skips credentials whose refresh is actively holding
  # its row lock. A credential can still be enqueued by multiple poll ticks
  # while the first job waits in the queue; BrokerCredential#refresh! re-checks
  # next_attempt_at under that same row lock so those stale jobs are no-ops.
  class PollRefreshJob < ApplicationJob
    queue_as :default

    def perform
      ids = BrokerCredential.transaction do
        BrokerCredential.refreshable.lock("FOR UPDATE SKIP LOCKED").pluck(:id)
      end
      ids.each { |id| Broker::RefreshCredentialJob.perform_later(id) }
    end
  end
end
