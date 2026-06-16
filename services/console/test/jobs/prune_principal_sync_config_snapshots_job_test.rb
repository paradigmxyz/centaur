require "test_helper"

class PrunePrincipalSyncConfigSnapshotsJobTest < ActiveJob::TestCase
  test "deletes snapshots older than retention" do
    principal = principals(:acme_channel)
    old = PrincipalSyncConfigSnapshot.create!(
      principal: principal,
      principal_cache_version: 10,
      payload: Principal::EMPTY_CONFIG,
      created_at: PrincipalSyncConfigSnapshot::RETENTION.ago - 1.minute,
      updated_at: PrincipalSyncConfigSnapshot::RETENTION.ago - 1.minute
    )
    fresh = PrincipalSyncConfigSnapshot.create!(
      principal: principal,
      principal_cache_version: 11,
      payload: Principal::EMPTY_CONFIG
    )

    PrunePrincipalSyncConfigSnapshotsJob.perform_now

    assert_not PrincipalSyncConfigSnapshot.exists?(old.id)
    assert PrincipalSyncConfigSnapshot.exists?(fresh.id)
  end
end
