require "test_helper"

class Api::V1::SandboxAdminControllerTest < ActionDispatch::IntegrationTest
  setup do
    @channel = principals(:acme_channel)
    @role = roles(:acme_admin_role)
  end

  test "an admin's Slack DM sandbox finds a channel principal and changes its roles" do
    proxy = dm_proxy_for(users(:acme_admin))

    with_token(proxy) do |headers|
      get "/api/v1/sandbox/admin/principals", params: { slack_channel_id: "c0123456789" }, headers: headers
      assert_response :ok
      assert_equal [ @channel.oid ], json_body.fetch("data").map { |principal| principal["id"] }

      get "/api/v1/sandbox/admin/roles/lookup/#{@role.foreign_id}", headers: headers
      assert_response :ok
      role_id = json_body.dig("data", "id")

      post "/api/v1/sandbox/admin/principals/#{@channel.oid}/roles",
           params: { data: { role_id: role_id } }, headers: headers, as: :json
      assert_response :created
      assert_includes @channel.reload.roles, @role

      delete "/api/v1/sandbox/admin/principals/#{@channel.oid}/roles/#{role_id}", headers: headers
      assert_response :no_content
      assert_not_includes @channel.reload.roles, @role
    end
  end

  test "an admin's Slack DM sandbox finds a secret and grants it directly to a principal" do
    secret = static_secrets(:acme_staging_api_key)

    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      get "/api/v1/sandbox/admin/secrets", params: { q: secret.name }, headers: headers
      assert_response :ok
      listed = json_body.fetch("data").find { |row| row["id"] == secret.oid }
      assert_equal "static_secret_id", listed["grant_key"]
      assert_equal %w[description foreign_id grant_key id labels name], listed.keys.sort

      post "/api/v1/sandbox/admin/grants",
           params: { data: { principal_id: @channel.oid, static_secret_id: secret.oid } }, headers: headers, as: :json
      assert_response :created
      grant_id = json_body.dig("data", "id")
      assert_includes @channel.granted_static_secrets, secret
      assert_equal users(:acme_admin), Grant.find_by_oid!(grant_id).created_by

      get "/api/v1/sandbox/admin/principals/#{@channel.oid}/grants", headers: headers
      assert_includes json_body.fetch("data").map { |grant| grant["id"] }, grant_id

      delete "/api/v1/sandbox/admin/grants/#{grant_id}", headers: headers
      assert_response :no_content
      assert_not_includes @channel.reload.granted_static_secrets, secret
    end
  end

  test "an admin's Slack DM sandbox lists a principal's effective grants and their sources" do
    role_secret = static_secrets(:acme_prod_api_key)
    pages = []

    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      [ 1, 2 ].each do |page|
        get "/api/v1/sandbox/admin/principals/#{@channel.oid}/effective_grants",
            params: { page: page, limit: 2 }, headers: headers
        assert_response :ok
        pages << json_body
      end
    end

    assert_equal({ "page" => 1, "limit" => 2, "total" => 4, "total_pages" => 2 }, pages.first["meta"])
    rows = pages.flat_map { |body| body.fetch("data") }
    expected = [ static_secrets(:github_token_inject), static_secrets(:db_password_replace), role_secret,
                 pg_dsn_secrets(:acme_analytics_pg) ].map(&:oid)
    assert_equal expected.sort, rows.map { |row| row["id"] }.sort

    inherited = rows.find { |row| row["id"] == role_secret.oid }
    assert_equal "static_secret_id", inherited["grant_key"]
    assert_not inherited["direct"]
    assert_equal [ roles(:acme_infra).oid ], inherited["roles"].map { |role| role["id"] }
    assert rows.find { |row| row["id"] == static_secrets(:github_token_inject).oid }["direct"]
  end

  test "an admin's Slack DM sandbox grants a secret to a role and revokes it" do
    secret = static_secrets(:acme_staging_api_key)

    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      post "/api/v1/sandbox/admin/grants",
           params: { data: { role_id: @role.oid, static_secret_id: secret.oid } }, headers: headers, as: :json
      assert_response :created
      grant_id = json_body.dig("data", "id")
      assert_includes @role.reload.grants.map(&:static_secret), secret

      get "/api/v1/sandbox/admin/roles/#{@role.oid}/grants", headers: headers
      assert_response :ok
      assert_includes json_body.fetch("data").map { |grant| grant["id"] }, grant_id

      delete "/api/v1/sandbox/admin/grants/#{grant_id}", headers: headers
      assert_response :no_content
      assert_not_includes @role.reload.grants.map(&:static_secret), secret
    end

    with_token(dm_proxy_for(users(:member_user))) do |headers|
      post "/api/v1/sandbox/admin/grants",
           params: { data: { role_id: @role.oid, static_secret_id: secret.oid } }, headers: headers, as: :json
    end
    assert_response :forbidden
    assert_not_includes @role.reload.grants.map(&:static_secret), secret
  end

  test "an admin's Slack DM sandbox changes Slack permissions and sandbox capabilities" do
    @channel.update!(sandbox_sessions_read_enabled: false, sandbox_workflows_read_enabled: true)
    version = -> { @channel.reload.sync_config_cache_version }

    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      post "/api/v1/sandbox/admin/principals/#{@channel.oid}/slack_channel_permissions",
           params: { data: { channel_id: "C0999999999", upload_enabled: false } }, headers: headers, as: :json
      assert_response :created
      permission = @channel.slack_channel_permissions.find_by!(channel_id: "C0999999999")
      assert_not permission.upload_enabled
      assert permission.history_enabled

      assert_changes version do
        delete "/api/v1/sandbox/admin/principals/#{@channel.oid}/slack_channel_permissions/c0999999999",
               headers: headers
      end
      assert_response :no_content
      assert_not @channel.slack_channel_permissions.exists?(channel_id: "C0999999999")

      assert_changes version do
        patch "/api/v1/sandbox/admin/principals/#{@channel.oid}/sandbox_access",
              params: { data: { sandbox_sessions_read_enabled: true } }, headers: headers, as: :json
      end
      assert_response :ok
    end

    @channel.reload
    assert @channel.sandbox_sessions_read_enabled
    assert @channel.sandbox_workflows_read_enabled
    assert_equal "all", @channel.sandbox_repo_cache
  end

  test "sandbox access updates reject values that are not booleans or a repo cache mode" do
    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      [
        { sandbox_sessions_read_enabled: nil },
        { sandbox_sessions_read_enabled: "maybe" },
        { sandbox_repo_cache: true },
        {}
      ].each do |data|
        patch "/api/v1/sandbox/admin/principals/#{@channel.oid}/sandbox_access",
              params: { data: data }, headers: headers, as: :json
        assert_response :bad_request
      end

      patch "/api/v1/sandbox/admin/principals/#{@channel.oid}/sandbox_access",
            params: { data: { sandbox_repo_cache: "everything" } }, headers: headers, as: :json
      assert_response :unprocessable_entity
    end
    assert_equal "all", @channel.reload.sandbox_repo_cache
  end

  test "only a home-workspace Slack DM linked to an active admin can change roles" do
    admin = users(:acme_admin)
    proxies = {
      "non-admin DM" => dm_proxy_for(users(:member_user)),
      "Slack Connect DM" => dm_proxy_for(admin, slack_team_id: "T0EXTERNAL1"),
      "Console session" => proxy_for(admin, kind: :console_user, slack_team_id: nil),
      "admin-linked channel" => proxy_for(admin, kind: :slack_channel, slack_team_id: nil,
                                                 slack_user_id: nil, slack_channel_id: "C0555555555"),
      "unlinked channel" => Proxy.create!(name: "channel-proxy", principal: @channel,
                                          bearer_token_hash: Digest::SHA256.hexdigest("channel"))
    }

    proxies.each do |label, proxy|
      with_token(proxy) do |headers|
        post "/api/v1/sandbox/admin/principals/#{@channel.oid}/roles",
             params: { data: { role_id: @role.oid } }, headers: headers, as: :json
      end
      assert_response :forbidden, label
    end
    assert_not_includes @channel.reload.roles, @role
  end

  test "an admin's DM loses access once the admin is disabled or demoted" do
    admin = users(:acme_admin)
    proxy = dm_proxy_for(admin)

    [ { status: :disabled }, { status: :active, admin: false } ].each do |change|
      admin.update!(change)
      with_token(proxy) do |headers|
        post "/api/v1/sandbox/admin/principals/#{@channel.oid}/roles",
             params: { data: { role_id: @role.oid } }, headers: headers, as: :json
      end
      assert_response :forbidden
    end
    assert_not_includes @channel.reload.roles, @role
  end

  test "an admin's Slack DM sandbox adds and removes Slack Connect workspaces" do
    api = FakeSlackExternalTeamsApi.new(%w[T0PARTNER1])

    with_slack_external_teams_api(api) do
      with_token(dm_proxy_for(users(:acme_admin))) do |headers|
        get "/api/v1/sandbox/admin/slack_external_teams", headers: headers
        assert_response :ok
        assert_equal %w[T0PARTNER1], json_body.fetch("data")

        2.times do
          post "/api/v1/sandbox/admin/slack_external_teams",
               params: { data: { team_id: " e0grid " } }, headers: headers, as: :json
          assert_response :ok
          assert_equal %w[T0PARTNER1 E0GRID], json_body.fetch("data")
        end
        assert_equal 1, api.replacements

        delete "/api/v1/sandbox/admin/slack_external_teams/t0partner1", headers: headers
        assert_response :ok
        assert_equal %w[E0GRID], api.team_ids

        delete "/api/v1/sandbox/admin/slack_external_teams/T0PARTNER1", headers: headers
        assert_response :not_found

        api.error = CentaurApiClient::Error.new("invalid Slack team ids: C0CHANNEL", status: 400)
        post "/api/v1/sandbox/admin/slack_external_teams",
             params: { data: { team_id: "C0CHANNEL" } }, headers: headers, as: :json
        assert_response :unprocessable_entity
        assert_equal "invalid Slack team ids: C0CHANNEL", json_body.dig("error", "message")

        api.error = Errno::ECONNREFUSED.new
        get "/api/v1/sandbox/admin/slack_external_teams", headers: headers
        assert_response :service_unavailable
      end

      with_token(dm_proxy_for(users(:member_user))) do |headers|
        post "/api/v1/sandbox/admin/slack_external_teams",
             params: { data: { team_id: "T0OTHER" } }, headers: headers, as: :json
      end
      assert_response :forbidden
    end
    assert_equal %w[E0GRID], api.team_ids
  end

  test "an admin's Slack DM sandbox creates channel and user principals with session-derived identities" do
    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      post "/api/v1/sandbox/admin/principals",
           params: { data: { kind: "slack_channel", slack_channel_id: " c0new12345 " } }, headers: headers, as: :json
      assert_response :created
      channel = Principal.find_by_oid!(json_body.dig("data", "id"))
      assert_equal [ "slack-channel-c0new12345", "slack_channel", "C0NEW12345", nil ],
                   [ channel.foreign_id, channel.kind, channel.slack_channel_id, channel.slack_team_id ]
      assert_equal users(:acme_admin), channel.created_by
      permission = channel.slack_channel_permissions.sole
      assert_equal "C0NEW12345", permission.channel_id
      assert permission.upload_enabled && permission.download_enabled && permission.history_enabled

      post "/api/v1/sandbox/admin/principals",
           params: { data: { kind: "slack_dm", slack_user_id: "U0NEWUSER1" } }, headers: headers, as: :json
      assert_response :created
      user = Principal.find_by_oid!(json_body.dig("data", "id"))
      assert_equal [ "slack-user-t0123456789-u0newuser1", "slack_dm", "U0NEWUSER1", "T0123456789" ],
                   [ user.foreign_id, user.kind, user.slack_user_id, user.slack_team_id ]

      post "/api/v1/sandbox/admin/principals",
           params: { data: { kind: "slack_channel", slack_channel_id: "C0NEW12345" } }, headers: headers, as: :json
      assert_response :unprocessable_entity
    end
  end

  test "sandbox admin principal creation accepts only Slack channel and user identities" do
    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      [
        [ { kind: "console_user", slack_channel_id: "C0NEW12345" }, :bad_request ],
        [ { kind: "slack_channel", slack_channel_id: "D0NEW12345" }, :bad_request ],
        [ { kind: "slack_dm", slack_user_id: "C0NEW12345" }, :unprocessable_entity ]
      ].each do |data, status|
        assert_no_difference "Principal.count" do
          post "/api/v1/sandbox/admin/principals", params: { data: data }, headers: headers, as: :json
        end
        assert_response status, data.inspect
      end
    end

    with_token(dm_proxy_for(users(:member_user))) do |headers|
      post "/api/v1/sandbox/admin/principals",
           params: { data: { kind: "slack_channel", slack_channel_id: "C0NEW12345" } }, headers: headers, as: :json
    end
    assert_response :forbidden
    assert_not Principal.exists?(foreign_id: "slack-channel-c0new12345")
  end

  test "an admin's Slack DM sandbox creates a workflow principal with the WORKFLOW_PRINCIPAL foreign ID" do
    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      post "/api/v1/sandbox/admin/principals",
           params: { data: { kind: "workflow", workflow_name: "Nightly_Report" } }, headers: headers, as: :json
      assert_response :created
      principal = Principal.find_by_oid!(json_body.dig("data", "id"))
      assert_equal [ "workflow-nightly-report", "workflow", "Workflow Nightly_Report" ],
                   [ principal.foreign_id, principal.kind, principal.name ]
      assert_equal "Nightly_Report", principal.labels["workflow_name"]

      post "/api/v1/sandbox/admin/principals", params: { data: { kind: "workflow" } }, headers: headers, as: :json
      assert_response :bad_request
    end
  end

  test "an admin's Slack DM sandbox creates a Postgres DSN secret from another's source under another role" do
    original = pg_dsn_secrets(:acme_analytics_pg)

    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      get "/api/v1/sandbox/admin/pg_dsn_secrets/#{original.oid}", headers: headers
      assert_response :ok
      dsn = json_body.dig("data", "dsn")

      post "/api/v1/sandbox/admin/pg_dsn_secrets",
           params: { data: { foreign_id: "pg-analytics-writer", name: "analytics writer", database: "analytics",
                             role: "writer", dsn: dsn } },
           headers: headers, as: :json
      assert_response :created
      copy = PgDsnSecret.find_by_oid!(json_body.dig("data", "id"))
      assert_equal [ "writer", "env", { "var" => "PG_ANALYTICS_DSN" }, users(:acme_admin) ],
                   [ copy.role, copy.dsn_source.source_type, copy.dsn_source.config, copy.created_by ]

      patch "/api/v1/sandbox/admin/pg_dsn_secrets/#{copy.oid}", params: { data: { role: "auditor" } },
                                                                 headers: headers, as: :json
      assert_response :ok
      copy.reload
      assert_equal [ "auditor", "analytics", "env" ], [ copy.role, copy.database, copy.dsn_source.source_type ]
    end
  end

  test "an admin's Slack DM sandbox creates a GCP ID token secret from another's keyfile and edits its audience" do
    original = gcp_id_token_secrets(:acme_cloud_run)

    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      get "/api/v1/sandbox/admin/gcp_id_token_secrets/#{original.oid}", headers: headers
      assert_response :ok
      keyfile = json_body.dig("data", "keyfile")

      post "/api/v1/sandbox/admin/gcp_id_token_secrets",
           params: { data: { foreign_id: "cloud-run-other", name: "other cloud run", audience: "https://other.example",
                             keyfile: keyfile, rules: [ { host: "other.example" } ] } },
           headers: headers, as: :json
      assert_response :created
      copy = GcpIdTokenSecret.find_by_oid!(json_body.dig("data", "id"))
      assert_equal [ "env", { "var" => "CLOUD_RUN_SA_KEYFILE" }, users(:acme_admin) ],
                   [ copy.keyfile_source.source_type, copy.keyfile_source.config, copy.created_by ]

      patch "/api/v1/sandbox/admin/gcp_id_token_secrets/#{copy.oid}",
            params: { data: { audience: "https://renamed.example" } }, headers: headers, as: :json
      assert_response :ok
      copy.reload
      assert_equal [ "https://renamed.example", %w[other.example], "env" ],
                   [ copy.audience, copy.rules.map(&:host), copy.keyfile_source.source_type ]

      post "/api/v1/sandbox/admin/gcp_id_token_secrets",
           params: { data: { audience: "https://x.example", rules: [ { host: "x.example" } ],
                             keyfile: { source_type: "control_plane", secret: "pasted", config: {} } } },
           headers: headers, as: :json
      assert_response :forbidden
    end
  end

  test "an admin's Slack DM sandbox edits a static secret's fields and keeps its source" do
    secret = static_secrets(:acme_staging_api_key)
    secret.create_source!(source_type: "control_plane", secret: "value-set-by-operator")

    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      patch "/api/v1/sandbox/admin/static_secrets/#{secret.oid}",
            params: { data: { description: "Partner API", rules: [ { host: "api.partner.example" } ] } },
            headers: headers, as: :json
      assert_response :ok
      secret.reload
      assert_equal [ "Partner API", { "header" => "X-Api-Key", "formatter" => "{{ .Value }}" }, %w[api.partner.example] ],
                   [ secret.description, secret.inject_config, secret.rules.map(&:host) ]
      assert_equal "control_plane", secret.source.source_type

      patch "/api/v1/sandbox/admin/static_secrets/#{secret.oid}",
            params: { data: { source: { source_type: "aws_sm", config: { secret_id: "partner/api-key" } } } },
            headers: headers, as: :json
      assert_response :ok
      assert_equal [ "aws_sm", { "secret_id" => "partner/api-key" } ], [ secret.reload.source.source_type, secret.source.config ]
    end
  end

  test "sandbox admin secret writes refuse inline values and OAuth credentials" do
    secret = static_secrets(:acme_staging_api_key)
    oauth_secret = static_secrets(:acme_managed_gmail_secret)

    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      [
        { source: { source_type: "control_plane", secret: "pasted", config: {} } },
        { source: { source_type: "token_broker", config: { credential_id: "managed-gmail" } } }
      ].each do |data|
        patch "/api/v1/sandbox/admin/static_secrets/#{secret.oid}", params: { data: data }, headers: headers, as: :json
        assert_response :forbidden, data.inspect
      end
      assert_nil secret.reload.source

      patch "/api/v1/sandbox/admin/static_secrets/#{oauth_secret.oid}",
            params: { data: { rules: [ { host: "elsewhere.example" } ] } }, headers: headers, as: :json
      assert_response :forbidden
    end

    with_token(dm_proxy_for(users(:member_user))) do |headers|
      patch "/api/v1/sandbox/admin/static_secrets/#{secret.oid}",
            params: { data: { description: "changed" } }, headers: headers, as: :json
    end
    assert_response :forbidden
    assert_equal "Staging API key", secret.reload.description
  end

  test "admin endpoints reject requests without a sandbox token" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      get "/api/v1/sandbox/admin/roles", headers: { "Authorization" => "Bearer iak_acme-ci-token" }
    end

    assert_response :unauthorized
  end

  private

  # Stands in for api-rs, which stores the allowlist.
  class FakeSlackExternalTeamsApi
    attr_reader :team_ids, :replacements
    attr_writer :error

    def initialize(team_ids)
      @team_ids = team_ids
      @replacements = 0
    end

    def slack_external_team_ids
      raise @error if @error

      @team_ids
    end

    def replace_slack_external_team_ids(team_ids)
      raise @error if @error

      @replacements += 1
      @team_ids = team_ids
    end
  end

  def with_slack_external_teams_api(api)
    controller = Api::V1::Sandbox::Admin::SlackExternalTeamsController
    original = controller.api_client_factory
    controller.api_client_factory = -> { api }
    yield
  ensure
    controller.api_client_factory = original
  end

  # The Slack channel catalog fixtures put the bot in T0123456789.
  def dm_proxy_for(user, slack_team_id: "T0123456789")
    proxy_for(user, kind: :slack_dm, slack_team_id: slack_team_id)
  end

  def proxy_for(user, kind:, slack_team_id:, slack_user_id: "U0123456789", slack_channel_id: nil)
    name = "#{kind}-#{user.id}-#{slack_team_id}"
    principal = Principal.create!(
      foreign_id: name.downcase,
      name: name,
      kind: kind,
      slack_user_id: slack_user_id,
      slack_team_id: slack_team_id,
      slack_channel_id: slack_channel_id,
      console_user: user,
      created_by: users(:acme_admin)
    )
    Proxy.create!(name: "proxy-#{name}".downcase, principal: principal,
                  bearer_token_hash: Digest::SHA256.hexdigest(name))
  end

  def with_token(proxy)
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      yield "Authorization" => "Bearer #{SandboxEntitlements::Jwt.encode_for_proxy(proxy)}"
    end
  end

  def json_body
    JSON.parse(response.body)
  end
end
