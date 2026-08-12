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

  def principal_with_grants(*grantables)
    principal = principals(:globex_user)
    grantables.each do |g|
      key = Grant::GRANTABLE_ASSOCIATIONS.find { |a| g.is_a?(a.to_s.camelize.constantize) }
      Grant.create!(principal: principal, key => g, created_by: users(:globex_admin))
    end
    principal
  end

  # Builds a static secret injecting `header` on `host`, granted directly to
  # globex_user (priority 100).
  def grant_direct_static(host:, header:)
    secret = StaticSecret.new(foreign_id: "static-#{SecureRandom.hex(4)}",
                              inject_config: { "header" => header, "formatter" => "Bearer {{ .Value }}" },
                              created_by: users(:globex_admin))
    secret.build_source(source_type: "control_plane", secret: "direct-token")
    secret.rules.build(host: host, position: 0)
    secret.save!
    Grant.create!(principal: principals(:globex_user), static_secret: secret, created_by: users(:globex_admin))
    secret
  end

  # Builds a gcp_auth secret (writes Authorization) matching `host`, granted to
  # globex_user through the globex_infra role (priority 0).
  def grant_role_gcp(host:)
    PrincipalRole.find_or_create_by!(principal: principals(:globex_user), role: roles(:globex_infra))
    secret = GcpAuthSecret.new(foreign_id: "gcp-#{SecureRandom.hex(4)}",
                               credentials_provider: { "type" => "workload_identity" },
                               scopes: [ "https://www.googleapis.com/auth/cloud-platform" ],
                               created_by: users(:globex_admin))
    secret.rules.build(host: host, position: 0)
    secret.save!
    Grant.create!(role: roles(:globex_infra), gcp_auth_secret: secret, created_by: users(:globex_admin))
    secret
  end

  def grant_direct_gcp(host:)
    secret = GcpAuthSecret.new(foreign_id: "gcp-#{SecureRandom.hex(4)}",
                               credentials_provider: { "type" => "workload_identity" },
                               scopes: [ "https://www.googleapis.com/auth/cloud-platform" ],
                               created_by: users(:globex_admin))
    secret.rules.build(host: host, position: 0)
    secret.save!
    Grant.create!(principal: principals(:globex_user), gcp_auth_secret: secret, created_by: users(:globex_admin))
    secret
  end

  def grant_role_oauth(secret = nil)
    PrincipalRole.find_or_create_by!(principal: principals(:globex_user), role: roles(:globex_infra))
    unless secret
      secret = OauthTokenSecret.new(
        foreign_id: "oauth-#{SecureRandom.hex(4)}",
        name: "gmail",
        grant: "refresh_token",
        token_endpoint: "https://oauth2.googleapis.com/token",
        scopes: [ "https://www.googleapis.com/auth/gmail.readonly" ],
        created_by: users(:globex_admin)
      )
      secret.sources.build(source_type: "1password", config: { "secret_ref" => "op://eng/gmail/refresh-token" },
                           role: "refresh_token", role_kind: "credential_field")
      secret.sources.build(source_type: "env", config: { "var" => "GMAIL_CLIENT_ID" },
                           role: "client_id", role_kind: "credential_field")
      secret.rules.build(host: "gmail.googleapis.com", http_methods: [ "GET" ], paths: [], position: 0)
      secret.save!
    end
    Grant.create!(role: roles(:globex_infra), oauth_token_secret: secret, created_by: users(:globex_admin))
    secret
  end

  def without_live_union_config
    original = PrincipalSyncConfigSnapshot.method(:live_union_config_for_proxy)
    PrincipalSyncConfigSnapshot.define_singleton_method(:live_union_config_for_proxy) do |*_args|
      raise "the union should not be assembled live without a requester"
    end
    yield
  ensure
    PrincipalSyncConfigSnapshot.define_singleton_method(:live_union_config_for_proxy, original)
    PrincipalSyncConfigSnapshot.private_class_method(:live_union_config_for_proxy)
  end

  def build_requester
    Principal.create!(foreign_id: "requester-#{SecureRandom.hex(4)}",
                      kind: "user", created_by: users(:globex_admin))
  end

  # Builds an always_available (unless overridden) OAuth app, a minted (unless
  # overridden) broker credential, its wrapper static secret injecting `header`
  # on `host`, and a grant: direct to `granted_to`, or via `via_role` when set.
  def build_hoistable_wrapper(granted_to:, host:, header: "Authorization", always_available: true,
                              minted: true, via_role: nil)
    app = OauthApp.create!(
      slug: "wrapper-#{SecureRandom.hex(4)}", provider: "github", client_id: "wrapper-client-id",
      client_secret: "wrapper-client-secret",
      allowed_scopes: [ "repo" ], always_available: always_available, created_by: users(:globex_admin)
    )
    cred = BrokerCredential.create!(
      foreign_id: "wrapper-cred-#{SecureRandom.hex(4)}",
      token_endpoint: "https://idp.example/token", client_id: "wrapper-client-id",
      refresh_token: "seed", oauth_app: app, created_by: users(:globex_admin)
    )
    cred.update!(access_token: "hoisted-token", expires_at: 1.hour.from_now, last_refresh: Time.current) if minted
    secret = StaticSecret.new(
      foreign_id: "wrapper-#{SecureRandom.hex(4)}",
      inject_config: { "header" => header, "formatter" => "Bearer {{ .Value }}" },
      broker_credential: cred, created_by: users(:globex_admin)
    )
    secret.build_source(source_type: "token_broker", config: { "credential_id" => cred.oid })
    secret.rules.build(host: host, position: 0)
    secret.save!
    if via_role
      PrincipalRole.find_or_create_by!(principal: granted_to, role: via_role)
      Grant.create!(role: via_role, static_secret: secret, created_by: users(:globex_admin))
    else
      Grant.create!(principal: granted_to, static_secret: secret, created_by: users(:globex_admin))
    end
    secret
  end

  test "config_for adds api server JWT from Slack channel permission rows" do
    with_env(
      "CENTAUR_JWT_SIGNING_SECRET" => "test-secret",
      "CENTAUR_API_URL" => "http://api.internal:8080",
      "CENTAUR_API_SERVER_PROXY_HOSTS" => nil
    ) do
      principal = principals(:acme_channel)
      SlackChannelPermission.create!(
        principal: principal,
        channel_id: "C0123456789",
        upload_enabled: true,
        download_enabled: false,
        history_enabled: true
      )
      SlackChannelPermission.create!(
        principal: principal,
        channel_id: "G9876543210",
        upload_enabled: false,
        download_enabled: true,
        history_enabled: false
      )

      config = PrincipalSyncConfigSnapshot.config_for(principal)
      entry = config.fetch("secrets").find do |secret|
        secret.dig("inject", "header") == "Authorization" &&
          secret.dig("source", "type") == "control_plane"
      end

      refute_nil entry
      assert_equal "Bearer {{ .Value }}", entry.dig("inject", "formatter")
      assert_equal [ { "host" => "api.internal" } ], entry.fetch("rules")

      claims = jwt_payload(entry.dig("source", "value"))
      assert_equal "centaur-console", claims.fetch("iss")
      assert_equal "centaur-api", claims.fetch("aud")
      assert_equal principal.oid, claims.fetch("sub")
      assert_equal [ "C0123456789" ], claims.dig("slack", "upload_channels")
      assert_equal [ "G9876543210" ], claims.dig("slack", "download_channels")
      assert_equal [ "C0123456789" ], claims.dig("slack", "history_channels")
      assert_equal 1.hour.to_i, claims.fetch("exp") - claims.fetch("iat")
      assert_equal ApiServer::Jwt.rotation_offset(principal),
                   claims.fetch("iat") % ApiServer::Jwt::DEFAULT_WINDOW_SECONDS
    end
  end

  test "config_for omits api server JWT when sandbox api access is disabled" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      principal = principals(:acme_channel)
      principal.update!(
        sandbox_api_server_enabled: false
      )
      SlackChannelPermission.create!(
        principal: principal,
        channel_id: "C0123456789",
        upload_enabled: true
      )

      config = PrincipalSyncConfigSnapshot.config_for(principal)
      entry = config.fetch("secrets").find do |secret|
        secret.dig("inject", "header") == "Authorization" &&
          secret.dig("source", "type") == "control_plane"
      end

      assert_nil entry
    end
  end

  test "sync_secrets delivers a brokered token inline and omits it until minted" do
    cred = BrokerCredential.create!(foreign_id: "sync-#{SecureRandom.hex(4)}",
                                    token_endpoint: "https://idp.example/token", client_id: "cid",
                                    created_by: users(:globex_admin), refresh_token: "seed")

    secret = StaticSecret.new(foreign_id: "brokered-#{SecureRandom.hex(4)}",
                              inject_config: { "header" => "Authorization" }, created_by: users(:globex_admin))
    secret.build_source(source_type: "token_broker", config: { "credential_id" => cred.oid })
    secret.rules.build(host: "api.example.com", position: 0)
    secret.save!

    principal = principal_with_grants(secret)

    # Bootstrapping (no token yet) -> the secret is omitted from sync entirely.
    assert_empty PrincipalSyncConfigSnapshot.sync_secrets_for(principal)

    # Once control mints a token, it is delivered inline like a control_plane value.
    cred.update!(access_token: "live-token", expires_at: 1.hour.from_now, last_refresh: Time.current)
    secrets = PrincipalSyncConfigSnapshot.sync_secrets_for(principal)
    assert_equal 1, secrets.length
    assert_equal({ "type" => "control_plane", "value" => "live-token" }, secrets.first["source"])

    # ...and redacted in the operator inspection view (no special-casing needed).
    redacted = PrincipalSyncConfigSnapshot.redacted_config_for(principal)
    assert_equal "[redacted]", redacted.dig("secrets", 0, "source", "value")
  end

  test "sync_transforms emits a gcp_auth transform per granted GcpAuthSecret" do
    transforms = PrincipalSyncConfigSnapshot.sync_transforms_for(principal_with_grants(gcp_auth_secrets(:acme_bigquery)))
    assert_equal 1, transforms.length
    assert_equal "gcp_auth", transforms.first["name"]
    assert_equal({ "type" => "workload_identity" }, transforms.first.dig("config", "credentials_provider"))
  end

  test "sync_transforms emits a gcp_id_token transform per granted GcpIdTokenSecret" do
    transforms = PrincipalSyncConfigSnapshot.sync_transforms_for(principal_with_grants(gcp_id_token_secrets(:acme_cloud_run)))
    assert_equal 1, transforms.length
    transform = transforms.first
    assert_equal "gcp_id_token", transform["name"]
    assert_equal({ "type" => "env", "var" => "CLOUD_RUN_SA_KEYFILE" }, transform.dig("config", "keyfile"))
    assert_equal "https://my-service-abc123-uc.a.run.app", transform.dig("config", "audience")
    assert_equal "x-serverless-authorization", transform.dig("config", "header")
  end

  test "sync_transforms emits an aws_auth transform per granted AwsAuthSecret" do
    transforms = PrincipalSyncConfigSnapshot.sync_transforms_for(principal_with_grants(aws_auth_secrets(:acme_cloudwatch_aws)))
    aws = transforms.find { |t| t["name"] == "aws_auth" }
    refute_nil aws
    assert_equal({ "type" => "env", "var" => "AWS_ACCESS_KEY_ID" }, aws.dig("config", "access_key_id"))
    assert_equal({ "type" => "env", "var" => "AWS_SECRET_ACCESS_KEY" }, aws.dig("config", "secret_access_key"))
    assert_equal %w[logs monitoring], aws.dig("config", "allowed_services")
    assert_equal 1, aws.dig("config", "rules").length
  end

  test "sync_transforms bundles all granted oauth tokens into one transform" do
    transforms = PrincipalSyncConfigSnapshot.sync_transforms_for(principal_with_grants(oauth_token_secrets(:acme_gmail_oauth)))
    oauth = transforms.find { |t| t["name"] == "oauth_token" }
    refute_nil oauth
    tokens = oauth.dig("config", "tokens")
    assert_equal 1, tokens.length
    assert_equal "refresh_token", tokens.first["grant"]
  end

  test "sync_transforms is empty without transform grants" do
    assert_empty PrincipalSyncConfigSnapshot.sync_transforms_for(principals(:globex_user))
  end

  test "sync_postgres emits a DSN entry per granted PgDsnSecret with foreign_id" do
    entries = PrincipalSyncConfigSnapshot.sync_postgres_for(principal_with_grants(pg_dsn_secrets(:acme_analytics_pg)))
    assert_equal 1, entries.length
    assert_equal pg_dsn_secrets(:acme_analytics_pg).foreign_id, entries.first["foreign_id"]
    assert_equal({ "type" => "env", "var" => "PG_ANALYTICS_DSN" }, entries.first["dsn"])
  end

  test "sync_postgres resolves value_from settings against the principal" do
    principal = principals(:globex_user)
    principal.update!(slack_channel_id: "C9999999999")
    pg = pg_dsn_secrets(:acme_analytics_pg)
    pg.update!(settings: [
      {
        "name" => "centaur.slack_channel_id",
        "value_from" => { "principal_field" => "slack_channel_id" }
      },
      { "name" => "centaur.principal", "value_from" => { "principal_field" => "foreign_id" } }
    ])
    Grant.create!(principal: principal, pg_dsn_secret: pg, created_by: users(:globex_admin))

    entry = PrincipalSyncConfigSnapshot.sync_postgres_for(principal).fetch(0)
    assert_equal(
      [
        { "name" => "centaur.slack_channel_id", "value" => "C9999999999" },
        { "name" => "centaur.principal", "value" => principal.foreign_id }
      ],
      entry["settings"]
    )
  end

  test "sync_postgres emits only the highest-priority route for each database" do
    principal = principals(:globex_user)
    low = pg_dsn_secrets(:acme_analytics_pg)
    high = PgDsnSecret.new(
      foreign_id: "pg-analytics-privileged",
      name: "analytics privileged",
      database: low.database,
      role: "centaur_readonly",
      created_by: users(:acme_admin)
    )
    high.build_dsn_source(source_type: "env", config: { "var" => "PG_PRIVILEGED_DSN" })
    high.save!

    Grant.create!(principal: principal, pg_dsn_secret: low, created_by: users(:acme_admin), priority: 0)
    Grant.create!(principal: principal, pg_dsn_secret: high, created_by: users(:acme_admin), priority: 100)

    entries = PrincipalSyncConfigSnapshot.sync_postgres_for(principal)
    assert_equal 1, entries.length
    assert_equal "pg-analytics-privileged", entries.first["foreign_id"]
    assert_equal "PG_PRIVILEGED_DSN", entries.first.dig("dsn", "var")
  end

  test "sync_postgres is empty without pg_dsn grants" do
    assert_empty PrincipalSyncConfigSnapshot.sync_postgres_for(principals(:globex_user))
  end

  test "a direct static secret suppresses a role-granted transform on the same host and header" do
    grant_direct_static(host: "api.test.com", header: "Authorization")
    grant_role_gcp(host: "api.test.com")
    principal = principals(:globex_user)

    assert_equal 1, PrincipalSyncConfigSnapshot.sync_secrets_for(principal).length
    assert_empty PrincipalSyncConfigSnapshot.sync_transforms_for(principal), "the lower-priority role gcp_auth should be withheld"
  end

  test "credentials writing different headers on the same host both serve" do
    grant_direct_static(host: "api.test.com", header: "X-Api-Key")
    grant_role_gcp(host: "api.test.com")
    principal = principals(:globex_user)

    assert_equal 1, PrincipalSyncConfigSnapshot.sync_secrets_for(principal).length
    assert_equal 1, PrincipalSyncConfigSnapshot.sync_transforms_for(principal).count { |t| t["name"] == "gcp_auth" }
  end

  test "credentials writing the same header on different hosts both serve" do
    grant_direct_static(host: "api.test.com", header: "Authorization")
    grant_role_gcp(host: "other.test.com")
    principal = principals(:globex_user)

    assert_equal 1, PrincipalSyncConfigSnapshot.sync_secrets_for(principal).length
    assert_equal 1, PrincipalSyncConfigSnapshot.sync_transforms_for(principal).count { |t| t["name"] == "gcp_auth" }
  end

  test "same-priority credentials writing the same header on the same host both serve" do
    grant_direct_static(host: "api.test.com", header: "Authorization")
    grant_direct_gcp(host: "api.test.com")
    principal = principals(:globex_user)

    assert_equal 1, PrincipalSyncConfigSnapshot.sync_secrets_for(principal).length
    assert_equal 1, PrincipalSyncConfigSnapshot.sync_transforms_for(principal).count { |t| t["name"] == "gcp_auth" }
  end

  test "a wildcard static secret suppresses a role-granted transform on a matching exact host" do
    grant_direct_static(host: "*.test.com", header: "Authorization")
    grant_role_gcp(host: "api.test.com")
    principal = principals(:globex_user)

    assert_equal 1, PrincipalSyncConfigSnapshot.sync_secrets_for(principal).length
    assert_empty PrincipalSyncConfigSnapshot.sync_transforms_for(principal), "the lower-priority role gcp_auth should be withheld"
  end

  test "a wildcard googleapis static secret suppresses oauth token entries on matching exact hosts" do
    grant_direct_static(host: "*.googleapis.com", header: "Authorization")
    grant_role_oauth
    principal = principals(:globex_user)

    assert_equal 1, PrincipalSyncConfigSnapshot.sync_secrets_for(principal).length
    assert_empty PrincipalSyncConfigSnapshot.sync_transforms_for(principal), "the lower-priority google oauth_token should be withheld"
  end

  test "a higher-priority wildcard googleapis static secret suppresses all google auth transforms" do
    grant_direct_static(host: "*.googleapis.com", header: "Authorization")
    grant_role_gcp(host: "bigquery.googleapis.com")
    grant_role_oauth
    grant_role_gcp(host: "*.googleapis.com")
    principal = principals(:globex_user)

    assert_equal 1, PrincipalSyncConfigSnapshot.sync_secrets_for(principal).length
    assert_empty PrincipalSyncConfigSnapshot.sync_transforms_for(principal), "lower-priority google auth transforms should be withheld"
  end

  test "equal-priority google auth transforms all serve without a stronger wildcard static secret" do
    grant_role_gcp(host: "bigquery.googleapis.com")
    grant_role_oauth
    grant_role_gcp(host: "*.googleapis.com")
    principal = principals(:globex_user)

    transforms = PrincipalSyncConfigSnapshot.sync_transforms_for(principal)
    assert_equal 2, transforms.count { |t| t["name"] == "gcp_auth" }
    assert_equal 1, transforms.count { |t| t["name"] == "oauth_token" }
    assert_equal 1, transforms.find { |t| t["name"] == "oauth_token" }.dig("config", "tokens").length
  end

  test "a promoted role transform suppresses a lower-priority direct static secret" do
    grant_direct_static(host: "api.test.com", header: "Authorization")
    gcp = grant_role_gcp(host: "api.test.com")
    # Promote the role grant above the direct grant (priority 100): now the
    # transform wins the conflict and the static secret is withheld.
    Grant.find_by!(gcp_auth_secret: gcp).update!(priority: 900)
    principal = principals(:globex_user)

    assert_empty PrincipalSyncConfigSnapshot.sync_secrets_for(principal), "the now-lower-priority direct static secret should be withheld"
    assert_equal 1, PrincipalSyncConfigSnapshot.sync_transforms_for(principal).count { |t| t["name"] == "gcp_auth" }
  end

  test "snapshot config can redact inline control_plane values" do
    principal = principals(:acme_channel)
    SecretSource.create!(source_type: "control_plane", secret: "s3cr3t",
                         static_secret: static_secrets(:db_password_replace))

    redacted_config = PrincipalSyncConfigSnapshot.redacted_config_for(principal)
    redacted = redacted_config.fetch("secrets").find { |s| s.dig("source", "type") == "control_plane" }
    assert_equal "[redacted]", redacted.dig("source", "value")

    live = PrincipalSyncConfigSnapshot.config_for(principal)
                    .fetch("secrets").find { |s| s.dig("source", "type") == "control_plane" }
    assert_equal "s3cr3t", live.dig("source", "value")
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

    assert_enqueued_with(job: PrincipalSyncConfigSnapshotWarmJob, args: [ @principal.id ]) do
      assert_no_changes -> { snapshot.reload.updated_at } do
        served = PrincipalSyncConfigSnapshot.fetch_for(@principal)
        assert_equal snapshot.id, served.id
        refute served.fresh?
      end
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
      clear_enqueued_jobs

      travel_to current_time do
        assert_enqueued_with(job: PrincipalSyncConfigSnapshotWarmJob, args: [ @principal.id ]) do
          served = PrincipalSyncConfigSnapshot.fetch_for(@principal)
          assert_equal snapshot.id, served.id
          refute served.fresh_for?(@principal)
        end

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

  test "cache version bump does not enqueue a snapshot warm job" do
    version = @principal.sync_config_cache_version

    assert_no_enqueued_jobs only: PrincipalSyncConfigSnapshotWarmJob do
      Principal.bump_sync_config_cache_versions(@principal.id)
    end

    assert_equal version + 1, @principal.reload.sync_config_cache_version
  end

  test "relation cache version bump updates direct and role grantees without warm jobs" do
    secret = static_secrets(:acme_prod_api_key)
    Grant.create!(
      principal: principals(:acme_user_bob),
      static_secret: secret,
      created_by: users(:acme_admin)
    )
    affected = [
      principals(:acme_channel),
      principals(:acme_user_alice),
      principals(:acme_user_bob)
    ]
    unaffected = principals(:globex_user)
    versions = Principal.where(id: (affected + [ unaffected ]).map(&:id)).pluck(:id, :sync_config_cache_version).to_h
    clear_enqueued_jobs

    assert_no_enqueued_jobs only: PrincipalSyncConfigSnapshotWarmJob do
      Principal.bump_sync_config_cache_versions(Principal.effective_grantees_for_grantable(secret))
    end

    affected.each do |principal|
      assert_equal versions.fetch(principal.id) + 1, principal.reload.sync_config_cache_version
    end
    assert_equal versions.fetch(unaffected.id), unaffected.reload.sync_config_cache_version
  end

  test "fetch_for serves the previous-version snapshot after a cache version bump" do
    old = PrincipalSyncConfigSnapshot.fetch_for(@principal)
    Principal.bump_sync_config_cache_versions(@principal.id)
    @principal.reload
    clear_enqueued_jobs

    assert_enqueued_with(job: PrincipalSyncConfigSnapshotWarmJob, args: [ @principal.id ]) do
      assert_no_difference -> { PrincipalSyncConfigSnapshot.count } do
        served = PrincipalSyncConfigSnapshot.fetch_for(@principal)
        assert_equal old.id, served.id
        refute_equal @principal.sync_config_cache_version, served.principal_cache_version
      end
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

  test "role Slack permission changes rebuild snapshots with inherited JWT claims" do
    with_env(
      "CENTAUR_JWT_SIGNING_SECRET" => "test-secret",
      "CENTAUR_API_URL" => "http://api.internal:8080"
    ) do
      old = PrincipalSyncConfigSnapshot.fetch_for(@principal)
      old_version = @principal.sync_config_cache_version

      roles(:acme_infra).slack_channel_permissions.create!(
        channel_id: "C0123456789",
        upload_enabled: true,
        history_enabled: true
      )

      @principal.reload
      assert_operator @principal.sync_config_cache_version, :>, old_version

      PrincipalSyncConfigSnapshotWarmJob.perform_now(@principal.id)
      fresh = PrincipalSyncConfigSnapshot.fetch_for(@principal)
      refute_equal old.id, fresh.id
      token = fresh.config.fetch("secrets").find do |secret|
        secret.dig("inject", "header") == "Authorization"
      end.dig("source", "value")
      claims = jwt_payload(token)
      assert_equal [ "C0123456789" ], claims.dig("slack", "upload_channels")
      assert_equal [ "C0123456789" ], claims.dig("slack", "history_channels")
    end
  end

  test "role assignment and removal rebuild snapshots with changed inherited access" do
    with_env(
      "CENTAUR_JWT_SIGNING_SECRET" => "test-secret",
      "CENTAUR_API_URL" => "http://api.internal:8080"
    ) do
      role = roles(:acme_admin_role)
      role.slack_channel_permissions.create!(
        channel_id: "G9876543210",
        download_enabled: true
      )
      old = PrincipalSyncConfigSnapshot.fetch_for(@principal)

      assignment = @principal.principal_roles.create!(role: role)
      PrincipalSyncConfigSnapshotWarmJob.perform_now(@principal.id)
      assigned = PrincipalSyncConfigSnapshot.fetch_for(@principal.reload)
      refute_equal old.id, assigned.id
      token = assigned.config.fetch("secrets").find do |secret|
        secret.dig("inject", "header") == "Authorization"
      end.dig("source", "value")
      assert_equal [ "G9876543210" ], jwt_payload(token).dig("slack", "download_channels")

      assignment.destroy!
      PrincipalSyncConfigSnapshotWarmJob.perform_now(@principal.id)
      removed = PrincipalSyncConfigSnapshot.fetch_for(@principal.reload)
      refute_equal assigned.id, removed.id
      api_server_secrets = removed.config.fetch("secrets").select do |secret|
        secret.dig("inject", "header") == "Authorization"
      end
      assert_empty api_server_secrets
    end
  end

  test "fetch_for falls back to a blocking build on cold start" do
    assert_difference -> { PrincipalSyncConfigSnapshot.count }, 1 do
      snapshot = PrincipalSyncConfigSnapshot.fetch_for(@principal)
      assert_equal @principal.sync_config_cache_version, snapshot.principal_cache_version
    end
  end

  # --- requester principal union ------------------------------------------

  test "a proxy without a requester renders identically without assembling the union live" do
    grant_direct_static(host: "api.test.com", header: "Authorization")
    proxy = Proxy.create!(name: "no-requester", principal: principals(:globex_user))
    expected = proxy.sync_config_snapshot

    without_live_union_config do
      actual = proxy.reload.sync_config_snapshot
      assert_equal expected.fetch(:config), actual.fetch(:config)
      assert_equal expected.fetch(:config_hash), actual.fetch(:config_hash)
    end
  end

  test "a requester's always_available wrapper joins the union alongside conversation grants" do
    grant_direct_static(host: "conv.test.com", header: "Authorization")
    baseline = Proxy.create!(name: "baseline", principal: principals(:globex_user))
                    .sync_config_snapshot.fetch(:config)

    requester = build_requester
    build_hoistable_wrapper(granted_to: requester, host: "github.com")
    proxy = Proxy.create!(name: "union", principal: principals(:globex_user), requester_principal: requester)

    config = proxy.sync_config_snapshot.fetch(:config)
    hoisted = config.fetch("secrets").find { |s| s.dig("source", "value") == "hoisted-token" }
    refute_nil hoisted
    assert_equal({ "type" => "control_plane", "value" => "hoisted-token" }, hoisted["source"])
    assert_equal baseline.fetch("secrets"), config.fetch("secrets") - [ hoisted ]
    assert_equal baseline.fetch("transforms"), config.fetch("transforms")
    assert_equal baseline.fetch("postgres"), config.fetch("postgres")
  end

  test "a wrapper whose oauth app is not always_available does not hoist" do
    requester = build_requester
    build_hoistable_wrapper(granted_to: requester, host: "github.com", always_available: false)
    proxy = Proxy.create!(name: "not-whitelisted", principal: principals(:globex_user), requester_principal: requester)

    assert_empty proxy.sync_config_snapshot.fetch(:config).fetch("secrets")
  end

  test "a wrapper granted to the requester through a role does not hoist" do
    requester = build_requester
    build_hoistable_wrapper(granted_to: requester, host: "github.com", via_role: roles(:globex_infra))
    proxy = Proxy.create!(name: "role-granted", principal: principals(:globex_user), requester_principal: requester)

    assert_empty proxy.sync_config_snapshot.fetch(:config).fetch("secrets")
  end

  test "non-static secret kinds granted directly to the requester do not hoist" do
    requester = build_requester
    admin = users(:globex_admin)
    Grant.create!(principal: requester, gcp_auth_secret: gcp_auth_secrets(:acme_bigquery), created_by: admin)
    Grant.create!(principal: requester, oauth_token_secret: oauth_token_secrets(:acme_gmail_oauth), created_by: admin)
    Grant.create!(principal: requester, pg_dsn_secret: pg_dsn_secrets(:acme_analytics_pg), created_by: admin)
    proxy = Proxy.create!(name: "other-kinds", principal: principals(:globex_user), requester_principal: requester)

    config = proxy.sync_config_snapshot.fetch(:config)
    assert_empty config.fetch("secrets")
    assert_empty config.fetch("transforms")
    assert_empty config.fetch("postgres")
  end

  test "an unminted wrapper credential is omitted from the union" do
    requester = build_requester
    build_hoistable_wrapper(granted_to: requester, host: "github.com", minted: false)
    proxy = Proxy.create!(name: "bootstrapping", principal: principals(:globex_user), requester_principal: requester)

    assert_empty proxy.sync_config_snapshot.fetch(:config).fetch("secrets")
  end

  test "a plain static secret directly granted to the requester does not hoist" do
    requester = build_requester
    secret = StaticSecret.new(
      foreign_id: "plain-#{SecureRandom.hex(4)}",
      inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
      created_by: users(:globex_admin)
    )
    secret.build_source(source_type: "env", config: { "var" => "PLAIN_TOKEN" })
    secret.rules.build(host: "plain.example", position: 0)
    secret.save!
    Grant.create!(principal: requester, static_secret: secret, created_by: users(:globex_admin))
    proxy = Proxy.create!(name: "plain-static", principal: principals(:globex_user), requester_principal: requester)

    assert_empty proxy.sync_config_snapshot.fetch(:config).fetch("secrets")
  end

  test "a wrapper whose source points at a different credential does not hoist" do
    requester = build_requester
    secret = build_hoistable_wrapper(granted_to: requester, host: "github.com")
    other = BrokerCredential.create!(
      foreign_id: "other-cred-#{SecureRandom.hex(4)}",
      token_endpoint: "https://idp.example/token", client_id: "other-client-id",
      refresh_token: "seed", created_by: users(:globex_admin)
    )
    other.update!(access_token: "other-token", expires_at: 1.hour.from_now, last_refresh: Time.current)
    secret.source.update!(config: { "credential_id" => other.oid })
    proxy = Proxy.create!(name: "mismatched-source", principal: principals(:globex_user), requester_principal: requester)

    assert_empty proxy.sync_config_snapshot.fetch(:config).fetch("secrets")
  end

  test "a requester wrapper suppresses a conversation role-granted transform on the same host and header" do
    grant_role_gcp(host: "api.github.com")
    requester = build_requester
    build_hoistable_wrapper(granted_to: requester, host: "api.github.com", header: "Authorization")
    proxy = Proxy.create!(name: "suppression", principal: principals(:globex_user), requester_principal: requester)

    config = proxy.sync_config_snapshot.fetch(:config)
    assert_equal 1, config.fetch("secrets").length
    assert_empty config.fetch("transforms"), "the lower-priority role gcp_auth should be withheld"

    proxy.update!(requester_principal: nil)
    config = proxy.reload.sync_config_snapshot.fetch(:config)
    assert_empty config.fetch("secrets")
    assert_equal 1, config.fetch("transforms").count { |t| t["name"] == "gcp_auth" }
  end

  test "a secret granted to both principals collapses to one entry at the strongest priority" do
    requester = build_requester
    secret = build_hoistable_wrapper(granted_to: requester, host: "github.com", header: "Authorization")
    Grant.create!(principal: principals(:globex_user), static_secret: secret,
                  created_by: users(:globex_admin), priority: 0)
    gcp = grant_role_gcp(host: "github.com")
    Grant.find_by!(gcp_auth_secret: gcp).update!(priority: 50)
    proxy = Proxy.create!(name: "both-pools", principal: principals(:globex_user), requester_principal: requester)

    config = proxy.sync_config_snapshot.fetch(:config)
    assert_equal 1, config.fetch("secrets").length
    assert_empty config.fetch("transforms"),
                 "the wrapper should carry the requester's direct priority, beating the promoted role grant"
  end

  test "config_hash distinguishes requester identity even when configs coincide" do
    proxy = Proxy.create!(name: "hash-identity", principal: principals(:globex_user),
                          requester_principal: build_requester)
    hash_a = proxy.sync_config_snapshot.fetch(:config_hash)

    proxy.update!(requester_principal: build_requester)
    hash_b = proxy.sync_config_snapshot.fetch(:config_hash)
    refute_equal hash_a, hash_b

    proxy.update!(requester_principal: nil)
    refute_equal hash_b, proxy.sync_config_snapshot.fetch(:config_hash)
  end

  test "a requester on an unassigned proxy still renders the empty config" do
    requester = build_requester
    build_hoistable_wrapper(granted_to: requester, host: "github.com")
    proxy = Proxy.create!(name: "unassigned-with-requester", principal: nil, requester_principal: requester)

    config = proxy.sync_config_snapshot.fetch(:config)
    assert_empty config.fetch("secrets")
    assert_empty config.fetch("transforms")
    assert_empty config.fetch("postgres")
  end

  def jwt_payload(token)
    _header, payload, _signature = token.split(".")
    JSON.parse(Base64.urlsafe_decode64(payload))
  end
end
