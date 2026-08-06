require "test_helper"
require Rails.root.join("db/migrate/20260806181422_remove_resource_namespaces")

class RemoveResourceNamespacesTest < ActiveSupport::TestCase
  test "invalidates cached sync config snapshots" do
    principal = principals(:acme_channel)
    PrincipalSyncConfigSnapshot.create!(
      principal: principal,
      principal_cache_version: principal.sync_config_cache_version,
      payload: { "config" => {} }
    )

    assert_difference -> { PrincipalSyncConfigSnapshot.count } => -1 do
      RemoveResourceNamespaces.new.send(:invalidate_sync_config_snapshots!)
    end
  end
end
