require "test_helper"

module Broker
  class JobsTest < ActiveJob::TestCase
    def make_credential(**overrides)
      BrokerCredential.create!({
        namespace: "default", foreign_id: "job-#{SecureRandom.hex(4)}",
        token_endpoint: "https://idp.example/token", client_id: "cid",
        created_by: users(:acme_admin), refresh_token: "seed"
      }.merge(overrides))
    end

    test "PollRefreshJob enqueues a refresh only for due credentials" do
      due = make_credential
      due.update_columns(next_attempt_at: 1.minute.ago)
      future = make_credential
      future.update_columns(next_attempt_at: 1.hour.from_now)

      Broker::PollRefreshJob.perform_now

      enqueued_ids = enqueued_jobs
        .select { |j| j[:job] == Broker::RefreshCredentialJob }
        .map { |j| j[:args].first }
      assert_includes enqueued_ids, due.id
      refute_includes enqueued_ids, future.id
    end

    test "RefreshCredentialJob drives the credential refresh" do
      # No refresh_token seed: refresh! marks the credential dead without any
      # network call, proving the job invoked it.
      bc = make_credential(refresh_token: nil)
      Broker::RefreshCredentialJob.perform_now(bc.id)
      bc.reload
      assert bc.dead?
      assert_equal "blob_not_bootstrapped", bc.dead_reason
    end

    test "RefreshCredentialJob is a no-op for a missing credential" do
      assert_nothing_raised { Broker::RefreshCredentialJob.perform_now(-1) }
    end
  end
end
