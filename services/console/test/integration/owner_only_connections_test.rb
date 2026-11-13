require "test_helper"

# Owner-only OAuth apps: each Console user connects their own account, which
# serves only that user's principals. Exercised with a custom MCP app, the
# shape the Runlayer preset creates.
class OwnerOnlyConnectionsTest < ActionDispatch::IntegrationTest
  ORIGIN = "https://mcp.example.test".freeze
  RESOURCE = "#{ORIGIN}/mcp".freeze
  CONFIG = { "authorization_endpoint" => "#{ORIGIN}/oauth/authorize", "token_endpoint" => "#{ORIGIN}/oauth/token",
             "revocation_endpoint" => "#{ORIGIN}/oauth/revoke", "resource" => RESOURCE }.freeze

  setup do
    @oauth_app = OauthApp.create!(slug: "personal-mcp", provider: "custom", provider_config: CONFIG, owner_only: true,
      always_available: true, labels: { "centaur-tool" => "personal-mcp" }, client_id: "registered-client",
      client_secret: "synthetic-client-secret", allowed_scopes: [ "mcp:proxy" ], created_by: users(:acme_admin))
    @alice = users(:member_user)
    @bob = users(:acme_admin)
    @alice_principal = principals(:acme_user_alice)
    @bob_principal = principals(:acme_user_bob)
    [ [ @alice, @alice_principal, "U0ALICE001" ], [ @bob, @bob_principal, "U0BOB00001" ] ].each do |user, principal, subject|
      UserIdentity.create!(user: user, provider: "slack", subject: subject, team_id: "T0WORK0001")
      principal.update!(slack_user_id: subject, slack_team_id: "T0WORK0001")
    end
    @proxy = proxies(:acme_proxy)
    @proxy.update!(principal: principals(:acme_channel), requester_principal: nil)
  end

  teardown do
    Api::V1::Console::OauthAppsController.registration_client_factory = -> { Oauth::DynamicRegistration.new }
    Oauth::FlowsController.exchange_client_factory = -> { Broker::AuthorizationCodeClient.new }
    Api::V1::Console::IntegrationsController.disconnect_service_factory = -> { Oauth::Disconnect.new }
  end

  test "OAuth connects two owners and each principal serves only its own token" do
    alice_credential = connect(@alice, "synthetic-alice")
    bob_credential = connect(@bob, "synthetic-bob")
    refute_equal alice_credential.id, bob_credential.id
    assert_equal "owner-#{@alice.oid}", alice_credential.provider_subject
    assert_nil alice_credential.provider_email
    assert_equal [ @alice_principal.id ], alice_credential.static_secret.grants.pluck(:principal_id)
    assert_equal [ @bob_principal.id ], bob_credential.static_secret.grants.pluck(:principal_id)
    assert_equal({ "centaur-tool" => "personal-mcp" }, alice_credential.static_secret.labels)
    [ [ @alice_principal, "synthetic-alice" ], [ @bob_principal, "synthetic-bob" ],
      [ principals(:acme_channel), nil ] ].each do |principal, expected|
      entries = owner_entries(principal)
      assert_equal expected ? [ expected ] : [], entries.map { |entry| entry.dig("source", "value") }
      entries.each { |entry| assert_equal [ "/mcp" ], entry["rules"].sole["paths"] }
    end
  end

  test "callback rejects a different signed in user before exchanging a code" do
    login(@alice)
    state = start_flow
    login(@bob)
    assert_no_difference "BrokerCredential.count" do
      get oauth_callback_url(slug: @oauth_app.slug), params: { state: state, code: "unused" }
    end
    assert_response :bad_request
  end

  test "callback rejects tampered states and endpoint changes since the flow started" do
    login(@alice)
    state = start_flow
    get oauth_callback_url(slug: @oauth_app.slug), params: { state: "tampered", code: "unused" }
    assert_response :bad_request
    @oauth_app.update!(provider_config: CONFIG.merge("token_endpoint" => "https://other.example.test/oauth/token"))
    assert_no_difference "BrokerCredential.count" do
      get oauth_callback_url(slug: @oauth_app.slug), params: { state: state, code: "unused" }
    end
    assert_response :bad_request
  end

  test "a mismatched wrapper source cannot substitute another owners token" do
    alice = connect(@alice, "synthetic-alice")
    bob = connect(@bob, "synthetic-bob")
    alice.static_secret.source.update!(config: { "credential_id" => bob.oid })
    assert_empty owner_entries
    connect(@alice, "synthetic-alice-reconnected")
    assert_equal [ "synthetic-alice-reconnected" ], owner_entries.map { |entry| entry.dig("source", "value") }
  end

  test "owner_only is fixed at creation and incompatible with shared apps" do
    @oauth_app.owner_only = false
    refute @oauth_app.valid?
    assert @oauth_app.errors[:owner_only].any?
    shared = OauthApp.new(slug: "shared-owner", provider: "custom", provider_config: CONFIG, owner_only: true, shared: true,
      client_id: "c", client_secret: "s", allowed_scopes: [ "mcp:proxy" ], created_by: users(:acme_admin))
    refute shared.valid?
    assert shared.errors[:owner_only].any?
  end

  test "connected credentials carry what token-broker needs to refresh with the resource" do
    credential = connect(@alice, "synthetic-before")
    row = ActiveRecord::Base.connection.select_one(<<~SQL)
      SELECT oauth_apps.provider, oauth_apps.provider_config ->> 'resource' AS resource, broker_credentials.token_endpoint
      FROM broker_credentials JOIN oauth_apps ON oauth_apps.id = broker_credentials.oauth_app_id
      WHERE broker_credentials.id = #{credential.id}
    SQL
    assert_equal({ "provider" => "custom", "resource" => RESOURCE, "token_endpoint" => CONFIG["token_endpoint"] }, row)
    assert credential.refresh_token.present?
    assert credential.next_attempt_at
  end

  test "disconnect is owner scoped and removes delivery even when remote revoke fails" do
    credential = connect(@alice, "synthetic-alice")
    login(@bob)
    delete connection_api_v1_console_integrations_path(credential.oid), headers: csrf_headers
    assert_response :not_found
    refute credential.reload.dead?
    login(@alice)
    http = HttpClient.new(http: ->(**) { raise Timeout::Error })
    Api::V1::Console::IntegrationsController.disconnect_service_factory = -> { Oauth::Disconnect.new(http: http) }
    delete connection_api_v1_console_integrations_path(credential.oid), headers: csrf_headers
    assert_response :ok
    assert_equal "failed", json_body.dig("data", "outcome")
    assert credential.reload.dead?
    assert_nil credential.refresh_token
    assert_empty owner_entries
    connect(@alice, "synthetic-reconnected")
    assert_equal [ "synthetic-reconnected" ], owner_entries.map { |entry| entry.dig("source", "value") }
  end

  test "reconnect reloads a credential disconnected after the callback reads it" do
    credential = connect(@alice, "synthetic-before")
    lookup = BrokerCredential.method(:find_or_initialize_by)
    revoke_http = HttpClient.new(http: ->(**) { HttpClient::Response.new(status: 200, body: "") })
    interleaved = false
    # Deterministically reproduce the concurrent ordering: the callback reads
    # an active row, disconnect updates it, then the callback resumes with its
    # stale instance. No sleeps or timing-dependent thread scheduling needed.
    interleave_disconnect = lambda do |**attributes|
      stale = lookup.call(**attributes)
      assert_equal credential.id, stale.id
      refute stale.dead?
      Oauth::Disconnect.new(http: revoke_http).call(BrokerCredential.find(credential.id))
      assert credential.reload.dead?
      interleaved = true
      stale
    end

    BrokerCredential.stub(:find_or_initialize_by, interleave_disconnect) do
      connect(@alice, "synthetic-after")
    end

    assert interleaved
    credential.reload
    refute credential.dead?
    assert_nil credential.dead_reason
    assert_equal "synthetic-after", credential.access_token
    assert_equal "synthetic-refresh", credential.refresh_token
    assert credential.next_attempt_at
    assert_equal [ "synthetic-after" ], owner_entries.map { |entry| entry.dig("source", "value") }
  end

  test "disconnect revokes both tokens and leaves nothing for token-broker to refresh" do
    credential = connect(@alice, "synthetic-alice")
    seen = []
    transport = ->(**request) do
      assert_equal CONFIG["revocation_endpoint"], request[:url]
      seen << URI.decode_www_form(request[:body]).to_h.fetch("token_type_hint")
      HttpClient::Response.new(status: 200, body: "")
    end
    assert_equal :revoked, Oauth::Disconnect.new(http: HttpClient.new(http: transport)).call(credential)
    assert_equal %w[refresh_token access_token], seen
    credential.reload
    # token-broker only refreshes live credentials (dead = false).
    assert credential.dead?
    assert_nil credential.refresh_token
    assert_nil credential.next_attempt_at
  end

  test "disconnect without a revocation endpoint removes local access only" do
    @oauth_app.update!(provider_config: CONFIG.except("revocation_endpoint"))
    credential = connect(@alice, "synthetic-alice")
    transport = ->(**) { flunk "no revocation endpoint is configured" }
    assert_equal :local_only, Oauth::Disconnect.new(http: HttpClient.new(http: transport)).call(credential)
    assert credential.reload.dead?
    assert_empty owner_entries
  end

  test "no role grants email matching expired disabled or dead credentials" do
    credential = connect(@alice, "synthetic-alice")
    @bob_principal.update!(labels: { "email" => @alice.email })
    PrincipalCredentialReconciliation.new.apply_for_principal(@bob_principal)
    refute credential.static_secret.grants.exists?(principal: @bob_principal)
    refute Grant.new(role: roles(:acme_infra), static_secret: credential.static_secret, created_by: @alice).valid?
    refute Grant.new(principal: @bob_principal, static_secret: credential.static_secret, created_by: @alice).valid?
    credential.update!(expires_at: 1.minute.ago)
    assert_empty owner_entries
    credential.update!(expires_at: 1.hour.from_now, dead: true)
    assert_empty owner_entries
    credential.update!(dead: false)
    @oauth_app.update!(enabled: false)
    assert_empty owner_entries
    @oauth_app.update!(enabled: true)
    @alice.update!(status: :disabled)
    assert_empty owner_entries
    # Disabling the owner withholds delivery without making the grant unsaveable.
    grant = credential.static_secret.grants.sole
    assert grant.update(priority: grant.priority + 1)
  end

  test "catalog exposes only public connection metadata and consent states are single use" do
    credential = connect(@alice, "synthetic-alice")
    assert_no_difference "BrokerCredential.count" do
      get oauth_callback_url(slug: @oauth_app.slug), params: { state: @last_state, code: "replayed" }
    end
    assert_response :bad_request
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "synthetic-signing-key") do
      token = SandboxEntitlements::Jwt.encode_for_proxy(@proxy)
      get "/api/v1/sandbox/oauth_apps", headers: { "Authorization" => "Bearer #{token}" }
    end
    assert_response :ok
    app = JSON.parse(response.body).fetch("data").find { |row| row["slug"] == @oauth_app.slug }
    assert_equal RESOURCE, app.fetch("resource")
    assert app.fetch("owner_only")
    refute app.key?("client_secret")
    refute app.key?("client_id")
    refute app.key?("provider_config")
    login(@alice)
    get api_v1_console_integrations_path
    entry = json_body.dig("data", "apps").find { |row| row["slug"] == @oauth_app.slug }
    assert entry["owner_only"]
    assert_equal({ "id" => credential.oid, "status" => "connected", "can_disconnect" => true }, entry["connection"])
    # Other users neither see nor may disconnect Alice's connection.
    login(@bob)
    get api_v1_console_integrations_path
    assert_nil json_body.dig("data", "apps").find { |row| row["slug"] == @oauth_app.slug }["connection"]
  end

  test "a disconnected connection no longer shows as connected" do
    credential = connect(@alice, "synthetic-alice")
    Oauth::Disconnect.new(http: HttpClient.new(http: ->(**) { HttpClient::Response.new(status: 200, body: "") })).call(credential)
    login(@alice)
    get api_v1_console_integrations_path
    assert_nil json_body.dig("data", "apps").find { |row| row["slug"] == @oauth_app.slug }["connection"]
  end

  test "denied consent does not create a credential" do
    login(@alice)
    state = start_flow
    assert_no_difference "BrokerCredential.count" do
      get oauth_callback_url(slug: @oauth_app.slug), params: { state: state, error: "access_denied" }
    end
    assert_response :unprocessable_entity
  end

  test "the console API sets owner_only only at creation" do
    login(@bob)
    patch api_v1_console_oauth_app_path(@oauth_app.oid), params: { data: { owner_only: false } }, headers: csrf_headers, as: :json
    assert_response :unprocessable_entity
    assert @oauth_app.reload.owner_only?
    get api_v1_console_oauth_app_path(@oauth_app.oid)
    assert json_body.dig("data", "owner_only")
    get api_v1_console_oauth_providers_path
    custom = json_body.fetch("data").find { |provider| provider["key"] == "custom" }
    assert_includes custom["optional_config_keys"], "revocation_endpoint"
  end

  test "Runlayer preset registers an owner-only custom app for the tenant the admin enters" do
    login(@bob)
    get api_v1_console_integrations_path
    preset = json_body.dig("data", "presets").find { |row| row["provider"] == "runlayer" }
    assert_equal [ "resource_url" ], preset["inputs"].pluck("name")
    calls = []
    transport = ->(**request) do
      calls << request
      assert_equal "https://tenant.runlayer.test/api/v1/oauth/register", request[:url]
      body = JSON.parse(request[:body])
      assert_equal "client_secret_post", body["token_endpoint_auth_method"]
      assert_equal "mcp:proxy", body["scope"]
      HttpClient::Response.new(status: 201, body: { client_id: "registered-client", client_secret: "synthetic-client-secret" }.to_json)
    end
    Api::V1::Console::OauthAppsController.registration_client_factory = -> { Oauth::DynamicRegistration.new(http: HttpClient.new(http: transport)) }
    [ nil, "http://tenant.runlayer.test/mcp", "https://tenant.runlayer.test/other", "https://u:p@tenant.runlayer.test/mcp",
      "https://tenant.runlayer.test/mcp?x=1", "https://tenant.runlayer.test:8443/mcp" ].each do |url|
      assert_no_difference "OauthApp.count" do
        post preset_api_v1_console_oauth_apps_path(provider: "runlayer"), params: { data: { resource_url: url }.compact },
          headers: csrf_headers, as: :json
      end
      assert_response :unprocessable_entity
      assert_equal "Enter a valid Runlayer MCP URL.", json_body.dig("error", "message")
    end
    assert_empty calls
    assert_difference "OauthApp.count", 1 do
      post preset_api_v1_console_oauth_apps_path(provider: "runlayer"),
        params: { data: { resource_url: " https://tenant.runlayer.test/mcp " } }, headers: csrf_headers, as: :json
    end
    assert_response :ok
    assert_equal "http://www.example.com/oauth/runlayer/start", json_body.dig("data", "start_url")
    app = OauthApp.find_by!(slug: "runlayer")
    assert_equal "custom", app.provider
    assert app.owner_only?
    assert app.always_available?
    refute app.shared?
    assert_equal({ "centaur-tool" => "runlayer" }, app.labels)
    assert_equal({ "authorization_endpoint" => "https://tenant.runlayer.test/api/v1/oauth/authorize",
                   "token_endpoint" => "https://tenant.runlayer.test/api/v1/oauth/token",
                   "revocation_endpoint" => "https://tenant.runlayer.test/api/v1/oauth/revoke",
                   "resource" => "https://tenant.runlayer.test/mcp" }, app.provider_config)
    assert_no_difference "OauthApp.count" do
      post preset_api_v1_console_oauth_apps_path(provider: "runlayer"), headers: csrf_headers, as: :json
    end
    assert_response :ok
    assert_equal 1, calls.length
    get api_v1_console_integrations_path
    assert_nil json_body.dig("data", "presets").find { |row| row["provider"] == "runlayer" }
  end

  test "the Runlayer preset is admin only" do
    assert_admin_only_api :post, preset_api_v1_console_oauth_apps_path(provider: "runlayer"),
      params: { data: { resource_url: "https://tenant.runlayer.test/mcp" } }
  end

  test "disconnect works for any personal connection" do
    google = oauth_apps(:acme_google)
    credential = BrokerCredential.create!(oauth_app: google, created_by: @alice, provider_subject: "alice-google",
      foreign_id: "google-acme-alice", name: "Google – alice", token_endpoint: "https://oauth2.example.test/token",
      access_token: "synthetic-google", refresh_token: "synthetic-google-refresh", external_user_key: SecureRandom.hex(8))
    login(@alice)
    delete connection_api_v1_console_integrations_path(credential.oid), headers: csrf_headers
    assert_response :ok
    assert_equal "local_only", json_body.dig("data", "outcome")
    assert credential.reload.dead?
  end

  test "disconnect is open to active users and needs CSRF" do
    assert_active_user_api :delete, connection_api_v1_console_integrations_path("bcr_missing")
  end

  private

  def login(user)
    sign_in_console(user)
  end

  def start_flow
    get oauth_start_url(slug: @oauth_app.slug)
    assert_response :redirect
    query = URI.decode_www_form(URI.parse(response.location).query).to_h
    assert_equal RESOURCE, query["resource"]
    assert_equal "S256", query["code_challenge_method"]
    assert_equal "mcp:proxy", query["scope"]
    query.fetch("state")
  end

  def connect(user, access_token)
    login(user)
    state = start_flow
    @last_state = state
    transport = ->(**request) do
      assert_equal RESOURCE, request[:form]["resource"]
      assert_equal "authorization_code", request[:form]["grant_type"]
      assert request[:form]["code_verifier"].present?
      # An opaque (non-JWT) token: owner-only apps never decode it.
      HttpClient::Response.new(status: 200, body: { access_token: access_token,
        refresh_token: "synthetic-refresh", expires_in: 3600, scope: "mcp:proxy" }.to_json)
    end
    Oauth::FlowsController.exchange_client_factory = -> { Broker::AuthorizationCodeClient.new(http: transport) }
    get oauth_callback_url(slug: @oauth_app.slug), params: { state: state, code: "synthetic-code" }
    assert_redirected_to console_integrations_path
    BrokerCredential.find_by!(oauth_app: @oauth_app, created_by: user)
  end

  def owner_entries(principal = @alice_principal)
    PrincipalSyncConfig.config_for(principal.reload).fetch("secrets").select do |entry|
      entry["rules"].any? { |rule| rule["host"] == "mcp.example.test" }
    end
  end
end
