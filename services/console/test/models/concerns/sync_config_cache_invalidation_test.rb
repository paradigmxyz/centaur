require "test_helper"

class SyncConfigCacheInvalidationTest < ActiveSupport::TestCase
  include ActiveJob::TestHelper

  class TestRecord < ApplicationRecord
    self.table_name = "roles"

    include SyncConfigCacheInvalidation

    attr_accessor :affected_principal_ids

    private

    def sync_config_affected_principal_ids
      affected_principal_ids
    end
  end

  def build_test_record(principal)
    TestRecord.find(roles(:acme_infra).id).tap do |record|
      record.affected_principal_ids = [ principal.id ]
    end
  end

  test "unchanged save does not enqueue a snapshot warm job" do
    principal = principals(:acme_channel)
    record = build_test_record(principal)
    version = principal.reload.sync_config_cache_version
    clear_enqueued_jobs

    assert_no_enqueued_jobs only: PrincipalSyncConfigSnapshotWarmJob do
      record.save!
    end

    assert_equal version, principal.reload.sync_config_cache_version
  end

  test "a later touch does not hide an earlier invalidating save in the same transaction" do
    principal = principals(:acme_channel)
    record = build_test_record(principal)
    version = principal.reload.sync_config_cache_version

    TestRecord.transaction do
      record.update!(name: "Updated")
      record.touch
    end

    assert_equal version + 1, principal.reload.sync_config_cache_version
  end
end
