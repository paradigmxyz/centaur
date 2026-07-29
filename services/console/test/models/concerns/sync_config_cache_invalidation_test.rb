require "test_helper"

class SyncConfigCacheInvalidationTest < ActiveSupport::TestCase
  include ActiveJob::TestHelper

  test "unchanged save does not enqueue a snapshot warm job" do
    principal = principals(:acme_channel)
    record = static_secrets(:github_token_inject)
    version = principal.reload.sync_config_cache_version
    clear_enqueued_jobs

    assert_no_enqueued_jobs only: PrincipalSyncConfigSnapshotWarmJob do
      record.save!
    end

    assert_equal version, principal.reload.sync_config_cache_version
  end

  test "a later touch does not hide an earlier invalidating save in the same transaction" do
    principal = principals(:acme_channel)
    record = static_secrets(:github_token_inject)
    version = principal.reload.sync_config_cache_version

    StaticSecret.transaction do
      record.update!(description: "Updated")
      record.touch
    end

    assert_equal version + 1, principal.reload.sync_config_cache_version
  end
end
