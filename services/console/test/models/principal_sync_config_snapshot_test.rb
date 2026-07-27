require "test_helper"

class PrincipalSyncConfigSnapshotTest < ActiveSupport::TestCase
  include ActiveJob::TestHelper
  include ActiveSupport::Testing::TimeHelpers

  setup do
    @principal = principals(:acme_channel)
  end

  teardown do
    clear_enqueued_jobs
    clear_performed_jobs
  end

  def without_live_sync_postgres
    original = PrincipalSyncConfigSnapshot.method(:sync_postgres_for)
    PrincipalSyncConfigSnapshot.define_singleton_method(:sync_postgres_for) do |*_args|
      raise "live sync_postgres should not be called"
    end
    yield
  ensure
    PrincipalSyncConfigSnapshot.define_singleton_method(:sync_postgres_for, original)
  end

  test "fetch_for builds a snapshot on cold start" do
    assert_difference -> { PrincipalSyncConfigSnapshot.count }, 1 do
      snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
      assert_equal @principal.sync_config_cache_version, snapshot.principal_cache_version
      assert_equal PrincipalSyncConfigSnapshot.payload_for(@principal), snapshot.payload
      assert_equal PrincipalSyncConfigSnapshot.config_for(@principal), snapshot.config
      assert_equal({}, snapshot.postgres_setting_templates)
    end
  end

  test "proxy sync renders proxy labels from the snapshot without recomputing postgres" do
    pg = pg_dsn_secrets(:acme_analytics_pg)
    pg.update!(settings: [
      { "name" => "centaur.principal", "value_from" => { "principal_field" => "foreign_id" } },
      { "name" => "centaur.slack_user_id", "value_from" => { "proxy_label" => "centaur.slack_user_id" } }
    ])
    proxy = proxies(:acme_proxy)
    proxy.update!(labels: { "centaur.slack_user_id" => "U0123456789" })
    cached = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    assert cached.postgres_setting_templates.key?(pg.oid)
    refute cached.config.key?("postgres_setting_templates")

    without_live_sync_postgres do
      snapshot = proxy.reload.sync_config_snapshot
      entry = snapshot.fetch(:config).fetch("postgres").find { |item| item["foreign_id"] == pg.foreign_id }

      assert_equal(
        [
          { "name" => "centaur.principal", "value" => @principal.foreign_id },
          { "name" => "centaur.slack_user_id", "value" => "U0123456789" }
        ],
        entry.fetch("settings")
      )
    end
  end

  test "snapshot accessors read flat payloads created before the snapshot envelope" do
    config = { "secrets" => [], "transforms" => [], "postgres" => [] }
    snapshot = PrincipalSyncConfigSnapshot.new(payload: config)

    assert_equal config, snapshot.config
    assert_empty snapshot.postgres_setting_templates
  end

  test "fetch_for returns the fresh snapshot without rebuilding" do
    snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)

    assert_no_difference -> { PrincipalSyncConfigSnapshot.count } do
      assert_equal snapshot, PrincipalSyncConfigSnapshot.fetch_for(@principal)
    end
    assert_equal snapshot.updated_at, snapshot.reload.updated_at
  end

  test "fetch_for serves a snapshot stale past TTL" do
    snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    stale_time = (PrincipalSyncConfigSnapshot::TTL + 1.minute).ago
    snapshot.update_columns(updated_at: stale_time)

    assert_no_changes -> { snapshot.reload.updated_at } do
      served = PrincipalSyncConfigSnapshot.fetch_for(@principal)
      assert_equal snapshot.id, served.id
      refute served.fresh?
    end
  end

  test "warm job rebuilds a snapshot stale past TTL" do
    snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    stale_time = (PrincipalSyncConfigSnapshot::TTL + 1.minute).ago
    snapshot.update_columns(updated_at: stale_time)

    PrincipalSyncConfigSnapshotWarmJob.perform_now(@principal.id)

    assert_equal snapshot.id, snapshot.reload.id
    assert snapshot.fresh?
  end

  test "warm job rebuilds api server JWT snapshots when the jwt window advances" do
    with_env(
      "CENTAUR_JWT_SIGNING_SECRET" => "test-secret",
      "CENTAUR_API_URL" => "http://api.internal:8080"
    ) do
      SlackChannelPermission.create!(
        principal: @principal,
        channel_id: "C0123456789",
        upload_enabled: true
      )
      boundary = 1_700_001_000 + ApiServer::Jwt.rotation_offset(@principal)
      current_time = Time.zone.at(boundary + 60)
      previous_window_time = Time.zone.at(boundary - 60)
      proxy = proxies(:acme_proxy)

      snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
      original_hash = proxy.sync_config_snapshot.fetch(:config_hash)
      original_token = snapshot.config.fetch("secrets").find do |secret|
        secret.dig("inject", "header") == "Authorization"
      end.dig("source", "value")
      snapshot.update_columns(updated_at: previous_window_time)

      travel_to current_time do
        served = PrincipalSyncConfigSnapshot.fetch_for(@principal)
        assert_equal snapshot.id, served.id
        refute served.fresh_for?(@principal)

        PrincipalSyncConfigSnapshotWarmJob.perform_now(@principal.id)
        refreshed = snapshot.reload
        refreshed_token = refreshed.config.fetch("secrets").find do |secret|
          secret.dig("inject", "header") == "Authorization"
        end.dig("source", "value")

        assert_equal snapshot.id, refreshed.id
        assert refreshed.fresh_for?(@principal)
        refute_equal original_token, refreshed_token
        refute_equal original_hash, proxy.reload.sync_config_snapshot.fetch(:config_hash)
      end
    end
  end

  test "fetch_for does not rebuild api server JWT snapshots when sandbox api access is disabled" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      @principal.update!(
        sandbox_api_server_enabled: false
      )
      SlackChannelPermission.create!(
        principal: @principal,
        channel_id: "C0123456789",
        upload_enabled: true
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

  test "cache version bump enqueues a snapshot warm job" do
    assert_enqueued_with(job: PrincipalSyncConfigSnapshotWarmJob, args: [ @principal.id ]) do
      Principal.bump_sync_config_cache_versions(@principal.id)
    end
  end

  test "fetch_for serves the previous-version snapshot after a cache version bump" do
    old = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    Principal.bump_sync_config_cache_versions(@principal.id)
    @principal.reload

    assert_no_difference -> { PrincipalSyncConfigSnapshot.count } do
      served = PrincipalSyncConfigSnapshot.fetch_for(@principal)
      assert_equal old.id, served.id
      refute_equal @principal.sync_config_cache_version, served.principal_cache_version
    end
  end

  test "warm job builds a new snapshot after a cache version bump" do
    old = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    Principal.bump_sync_config_cache_versions(@principal.id)
    @principal.reload

    assert_difference -> { PrincipalSyncConfigSnapshot.count }, 1 do
      PrincipalSyncConfigSnapshotWarmJob.perform_now(@principal.id)
    end
    fresh = PrincipalSyncConfigSnapshot.find_by!(principal: @principal, principal_cache_version: @principal.sync_config_cache_version)
    refute_equal old.id, fresh.id
  end

  test "fetch_for falls back to a blocking build on cold start" do
    assert_difference -> { PrincipalSyncConfigSnapshot.count }, 1 do
      snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
      assert_equal @principal.sync_config_cache_version, snapshot.principal_cache_version
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
