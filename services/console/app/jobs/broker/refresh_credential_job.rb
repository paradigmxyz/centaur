module Broker
  # Refreshes one BrokerCredential. limits_concurrency serializes runs per
  # credential at the queue layer so duplicate enqueues (e.g. two poll ticks)
  # don't pile up; BrokerCredential#refresh! takes a row lock as the real
  # single-writer guarantee.
  #
  # #refresh! never raises for an IdP or config failure -- it records the outcome
  # (backoff schedule or dead state) in the row -- so this job does not rely on
  # ActiveJob retry. The next poll tick picks up anything still due.
  class RefreshCredentialJob < ApplicationJob
    queue_as :default

    limits_concurrency to: 1, key: ->(credential_id) { "broker_refresh_#{credential_id}" }

    def perform(credential_id)
      credential = BrokerCredential.find_by(id: credential_id)
      return unless credential

      credential.refresh!
    end
  end
end
