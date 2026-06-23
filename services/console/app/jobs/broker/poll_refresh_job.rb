module Broker
  # The recurring driver of the refresh loop, replacing iron-token-broker's
  # per-credential goroutine. Level-triggered: every tick it re-derives which
  # credentials are due from the database, so a missed tick is caught by the
  # next one (unlike a self-rescheduling job, which orphans a credential forever
  # if its enqueue is ever lost).
  #
  # FOR UPDATE SKIP LOCKED skips credentials whose refresh is already in flight
  # (locked by a RefreshCredentialJob), so we don't enqueue redundant work for
  # them.
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
