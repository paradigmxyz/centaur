require "test_helper"

class PrincipalSyncConfigSnapshotTest < ActiveSupport::TestCase
  include ActiveSupport::Testing::TimeHelpers

  setup do
    @principal = principals(:acme_channel)
  end

  # Simulates losing the non-blocking rebuild race: another session holds the
  # principal row lock, so try_build_for's SKIP LOCKED select comes back empty.
  # (Real cross-session lock contention is not reproducible under transactional
  # tests, where all sessions share one connection.)
  def while_rebuild_lock_held
    singleton = PrincipalSyncConfigSnapshot.singleton_class
    original = PrincipalSyncConfigSnapshot.method(:try_build_for)
    singleton.define_method(:try_build_for) { |_principal| nil }
    yield
  ensure
    singleton.define_method(:try_build_for, original)
  end

  test "fetch_for builds a snapshot on cold start" do
    assert_difference -> { PrincipalSyncConfigSnapshot.count }, 1 do
      snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
      assert_equal @principal.sync_config_cache_version, snapshot.principal_cache_version
      assert_equal @principal.effective_config(redact_secrets: false), snapshot.payload
    end
  end

  test "fetch_for returns the fresh snapshot without rebuilding" do
    snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)

    assert_no_difference -> { PrincipalSyncConfigSnapshot.count } do
      assert_equal snapshot, PrincipalSyncConfigSnapshot.fetch_for(@principal)
    end
    assert_equal snapshot.updated_at, snapshot.reload.updated_at
  end

  test "fetch_for rebuilds a snapshot stale past TTL" do
    snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    stale_time = (PrincipalSyncConfigSnapshot::TTL + 1.minute).ago
    snapshot.update_columns(updated_at: stale_time)

    refreshed = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    assert_equal snapshot.id, refreshed.id
    assert refreshed.fresh?
  end

  test "fetch_for rebuilds api server JWT snapshots when the jwt window advances" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      @principal.update!(labels: { Principal::SLACK_CHANNEL_ID_LABEL => "C0123456789" })
      boundary = 1_700_001_000 + ApiServer::Jwt.rotation_offset(@principal)
      current_time = Time.zone.at(boundary + 60)
      previous_window_time = Time.zone.at(boundary - 60)
      proxy = proxies(:acme_proxy)

      snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
      original_hash = proxy.sync_config_snapshot.fetch(:config_hash)
      original_token = snapshot.payload.fetch("secrets").find do |secret|
        secret.dig("inject", "header") == "Authorization"
      end.dig("source", "value")
      snapshot.update_columns(updated_at: previous_window_time)

      travel_to current_time do
        refreshed = PrincipalSyncConfigSnapshot.fetch_for(@principal)
        refreshed_token = refreshed.payload.fetch("secrets").find do |secret|
          secret.dig("inject", "header") == "Authorization"
        end.dig("source", "value")

        assert_equal snapshot.id, refreshed.id
        assert refreshed.fresh?
        refute_equal original_token, refreshed_token
        refute_equal original_hash, proxy.reload.sync_config_snapshot.fetch(:config_hash)
      end
    end
  end

  test "fetch_for does not rebuild api server JWT snapshots when sandbox api access is disabled" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      @principal.update!(
        labels: { Principal::SLACK_CHANNEL_ID_LABEL => "C0123456789" },
        sandbox_api_server_enabled: false
      )
      boundary = 1_700_001_000 + ApiServer::Jwt.rotation_offset(@principal)
      current_time = Time.zone.at(boundary + 60)
      previous_window_time = Time.zone.at(boundary - 60)

      snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
      snapshot.update_columns(updated_at: previous_window_time)

      travel_to current_time do
        assert_no_changes -> { snapshot.reload.updated_at } do
          assert_equal snapshot, PrincipalSyncConfigSnapshot.fetch_for(@principal)
        end
      end
    end
  end

  test "fetch_for builds a new snapshot after a cache version bump" do
    old = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    Principal.bump_sync_config_cache_versions(@principal.id)
    @principal.reload

    fresh = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    refute_equal old.id, fresh.id
    assert_equal @principal.sync_config_cache_version, fresh.principal_cache_version
  end

  # The stampede regression: when another session holds the rebuild lock,
  # fetch_for must serve the stale current-version snapshot instead of
  # queuing behind the row lock.
  test "fetch_for serves the stale snapshot while another session rebuilds" do
    snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    stale_time = (PrincipalSyncConfigSnapshot::TTL + 1.minute).ago
    snapshot.update_columns(updated_at: stale_time)

    while_rebuild_lock_held do
      assert_no_difference -> { PrincipalSyncConfigSnapshot.count } do
        served = PrincipalSyncConfigSnapshot.fetch_for(@principal)
        assert_equal snapshot.id, served.id
        refute served.fresh?
      end
    end
  end

  test "fetch_for serves the previous-version snapshot while another session rebuilds after a bump" do
    old = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    Principal.bump_sync_config_cache_versions(@principal.id)
    @principal.reload

    while_rebuild_lock_held do
      assert_no_difference -> { PrincipalSyncConfigSnapshot.count } do
        served = PrincipalSyncConfigSnapshot.fetch_for(@principal)
        assert_equal old.id, served.id
        refute_equal @principal.sync_config_cache_version, served.principal_cache_version
      end
    end
  end

  test "fetch_for falls back to a blocking build on cold start when the non-blocking build loses" do
    while_rebuild_lock_held do
      assert_difference -> { PrincipalSyncConfigSnapshot.count }, 1 do
        snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
        assert_equal @principal.sync_config_cache_version, snapshot.principal_cache_version
      end
    end
  end

  test "try_build_for builds when the principal row lock is free" do
    assert_difference -> { PrincipalSyncConfigSnapshot.count }, 1 do
      snapshot = PrincipalSyncConfigSnapshot.try_build_for(@principal)
      assert_equal @principal.sync_config_cache_version, snapshot.principal_cache_version
    end
  end

  test "try_build_for returns the existing snapshot when already fresh" do
    snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)

    assert_no_difference -> { PrincipalSyncConfigSnapshot.count } do
      assert_equal snapshot, PrincipalSyncConfigSnapshot.try_build_for(@principal)
    end
  end

  def with_env(values)
    previous = values.keys.to_h { |key| [ key, ENV[key] ] }
    values.each do |key, value|
      value.nil? ? ENV.delete(key) : ENV[key] = value
    end
    yield
  ensure
    previous.each do |key, value|
      value.nil? ? ENV.delete(key) : ENV[key] = value
    end
  end
end
