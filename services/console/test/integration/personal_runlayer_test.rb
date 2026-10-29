require "test_helper"

class PersonalRunlayerTest < ActionDispatch::IntegrationTest
  RESOURCE = "https://runlayer.example.test/mcp"

  setup do
    @previous_resource_url = ENV["RUNLAYER_MCP_URL"]
    ENV["RUNLAYER_MCP_URL"] = RESOURCE
    @app = OauthApp.create!(slug: "runlayer", provider: "runlayer", client_id: "registered-client",
      client_secret: "synthetic-client-secret", allowed_scopes: [ "mcp:proxy" ],
      created_by: users(:acme_admin))
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
    @previous_resource_url.nil? ? ENV.delete("RUNLAYER_MCP_URL") : ENV["RUNLAYER_MCP_URL"] = @previous_resource_url
    Console::OauthAppsController.registration_client_factory = -> { Oauth::DynamicRegistration.new }
    Oauth::FlowsController.exchange_client_factory = -> { Broker::AuthorizationCodeClient.new }
    Console::IntegrationsController.disconnect_service_factory = -> { Oauth::DisconnectRunlayer.new }
  end

  test "OAuth connects two owners and each principal serves only its own token" do
    alice_credential = connect(@alice, "synthetic-alice")
    bob_credential = connect(@bob, "synthetic-bob")
    refute_equal alice_credential.id, bob_credential.id
    assert_equal [ @alice_principal.id ], alice_credential.static_secret.grants.pluck(:principal_id)
    assert_equal [ @bob_principal.id ], bob_credential.static_secret.grants.pluck(:principal_id)
    assert_equal({ "centaur-tool" => "runlayer" }, alice_credential.static_secret.labels)
    [ [ @alice_principal, "synthetic-alice" ], [ @bob_principal, "synthetic-bob" ],
      [ principals(:acme_channel), nil ] ].each do |principal, expected|
      entries = runlayer_entries(principal)
      assert_equal expected ? [ expected ] : [], entries.map { |entry| entry.dig("source", "value") }
      entries.each do |entry|
        assert_equal [ { "host" => "runlayer.example.test", "methods" => %w[GET POST DELETE], "paths" => [ "/mcp" ] } ], entry["rules"]
      end
    end
  end

  test "callback rejects a different signed in user before exchanging a code" do
    login(@alice)
    state = start_flow
    login(@bob)
    assert_no_difference "BrokerCredential.count" do
      get oauth_callback_url(slug: @app.slug), params: { state: state, code: "unused" }
    end
    assert_response :bad_request
  end

  test "a mismatched wrapper source cannot substitute another owners token" do
    alice = connect(@alice, "synthetic-alice")
    bob = connect(@bob, "synthetic-bob")
    alice.static_secret.source.update!(config: { "credential_id" => bob.oid })
    assert_empty runlayer_entries
    connect(@alice, "synthetic-alice-reconnected")
    assert_equal [ "synthetic-alice-reconnected" ], runlayer_entries.map { |entry| entry.dig("source", "value") }
  end

  test "callback rejects tenant changes and invalid states" do
    login(@alice)
    state = start_flow
    ENV["RUNLAYER_MCP_URL"] = "https://other.example.test/mcp"
    get oauth_callback_url(slug: @app.slug), params: { state: state, code: "unused" }
    assert_response :bad_request
    get oauth_callback_url(slug: @app.slug), params: { state: "tampered", code: "unused" }
    assert_response :bad_request
  end

  test "refresh carries the resource and rotates the stored token" do
    credential = connect(@alice, "synthetic-before")
    transport = ->(**request) do
      assert_equal RESOURCE, request[:form]["resource"]
      assert_equal "refresh_token", request[:form]["grant_type"]
      HttpClient::Response.new(status: 200, body: { access_token: "synthetic-after", refresh_token: "rotated", expires_in: 3600 }.to_json)
    end
    credential.update!(next_attempt_at: Time.current)
    credential.refresh_client = Broker::RefreshClient.new(http: transport)
    credential.refresh!
    assert_equal [ "synthetic-after" ], runlayer_entries.map { |entry| entry.dig("source", "value") }
    ENV["RUNLAYER_MCP_URL"] = "https://changed.example.test/mcp"
    credential.reload
    assert @app.credential_needs_reconnect?(credential)
    assert_empty runlayer_entries
    credential.update!(next_attempt_at: Time.current)
    credential.refresh!
    assert credential.dead?
    assert_empty runlayer_entries
  end

  test "missing or malformed deployment configuration defers refresh without killing the connection" do
    credential = connect(@alice, "synthetic-before")
    credential.refresh_client = Broker::RefreshClient.new(http: ->(**) { flunk "unconfigured provider must not contact upstream" })
    [ nil, "http://invalid.example.test/mcp" ].each do |url|
      with_env("RUNLAYER_MCP_URL" => url) do
        credential.update!(next_attempt_at: Time.current)
        credential.refresh!
        refute credential.dead?
        assert_equal "synthetic-before", credential.access_token
        assert_equal "synthetic-refresh", credential.refresh_token
        assert_operator credential.next_attempt_at, :>, Time.current
        assert_empty runlayer_entries
      end
    end
    credential.reload
    refute @app.credential_needs_reconnect?(credential)
    assert_equal [ "synthetic-before" ], runlayer_entries.map { |entry| entry.dig("source", "value") }
  end

  test "disconnect is owner scoped and removes delivery even when remote revoke fails" do
    credential = connect(@alice, "synthetic-alice")
    login(@bob)
    delete console_integration_connection_url(credential.oid)
    assert_response :not_found
    refute credential.reload.dead?
    login(@alice)
    http = HttpClient.new(http: ->(**) { raise Timeout::Error })
    Console::IntegrationsController.disconnect_service_factory = -> { Oauth::DisconnectRunlayer.new(http: http) }
    delete console_integration_connection_url(credential.oid)
    assert_redirected_to console_integrations_path
    assert credential.reload.dead?
    assert_nil credential.refresh_token
    assert_empty runlayer_entries
    connect(@alice, "synthetic-reconnected")
    assert_equal [ "synthetic-reconnected" ], runlayer_entries.map { |entry| entry.dig("source", "value") }
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
      Oauth::DisconnectRunlayer.new(http: revoke_http).call(BrokerCredential.find(credential.id))
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
    assert_equal [ "synthetic-after" ], runlayer_entries.map { |entry| entry.dig("source", "value") }
  end

  test "disconnect revokes both tokens and a queued refresh cannot reconnect" do
    credential = connect(@alice, "synthetic-alice")
    seen = []
    transport = ->(**request) do
      assert_equal "https://runlayer.example.test/api/v1/oauth/revoke", request[:url]
      seen << URI.decode_www_form(request[:body]).to_h.fetch("token_type_hint")
      HttpClient::Response.new(status: 200, body: "")
    end
    assert Oauth::DisconnectRunlayer.new(http: HttpClient.new(http: transport)).call(credential)
    assert_equal %w[refresh_token access_token], seen
    credential.refresh_client = Broker::RefreshClient.new(http: ->(**) { flunk "disconnected token must not refresh" })
    credential.refresh!
    assert credential.dead?
  end

  test "no role grants email matching expired disabled or dead credentials" do
    credential = connect(@alice, "synthetic-alice")
    @bob_principal.update!(labels: { "email" => @alice.email })
    PrincipalCredentialReconciliation.new.apply_for_principal(@bob_principal)
    refute credential.static_secret.grants.exists?(principal: @bob_principal)
    grant = Grant.new(role: roles(:acme_infra), static_secret: credential.static_secret, created_by: @alice)
    refute grant.valid?
    credential.update!(expires_at: 1.minute.ago)
    assert_empty runlayer_entries
    credential.update!(expires_at: 1.hour.from_now, dead: true)
    assert_empty runlayer_entries
    credential.update!(dead: false)
    @app.update!(enabled: false)
    assert_empty runlayer_entries
    @app.update!(enabled: true)
    @alice.update!(status: :disabled)
    assert_empty runlayer_entries
  end

  test "catalog exposes only public connection metadata and consent states are single use" do
    credential = connect(@alice, "synthetic-alice")
    assert_no_difference "BrokerCredential.count" do
      get oauth_callback_url(slug: @app.slug), params: { state: @last_state, code: "replayed" }
    end
    assert_response :bad_request
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "synthetic-signing-key") do
      token = SandboxEntitlements::Jwt.encode_for_proxy(@proxy)
      get "/api/v1/sandbox/oauth_apps", headers: { "Authorization" => "Bearer #{token}" }
    end
    assert_response :ok
    app = JSON.parse(response.body).fetch("data").find { |row| row["provider"] == "runlayer" }
    refute app.fetch("labels").key?("mcp_url")
    refute app.key?("client_secret")
    refute app.key?("client_id")
    login(@alice)
    get console_integrations_url
    assert_select "form[action=?]", console_integration_connection_path(credential.oid)
  end

  test "denied consent does not create a credential" do
    login(@alice)
    state = start_flow
    assert_no_difference "BrokerCredential.count" do
      get oauth_callback_url(slug: @app.slug), params: { state: state, error: "access_denied" }
    end
    assert_response :unprocessable_entity
  end

  test "a Runlayer app must be personal and have an exact configured resource" do
    @app.shared = true
    refute @app.valid?
    provider = @app.provider_strategy
    [ "http://runlayer.example.test/mcp", "https://user:password@example.test/mcp", "https://example.test/mcp?token=x", "https://example.test/*" ].each do |url|
      with_env("RUNLAYER_MCP_URL" => url) { refute provider.configured? }
    end
  end

  test "Runlayer uses deployment configuration and fixes personal requester availability" do
    @app.labels = { "mcp_url" => "https://ignored.example.test/mcp" }
    assert_equal RESOURCE, @app.provider_strategy.resource_url
    @app.always_available = false
    assert @app.valid?
    assert @app.always_available?
    with_env("RUNLAYER_MCP_URL" => nil) do
      refute @app.valid?
      login(@bob)
      get console_integrations_url
      assert_select "form[action=?]", console_oauth_app_preset_path(provider: "runlayer"), count: 0
    end
    login(@bob)
    get edit_console_oauth_app_form_path(@app.oid)
    assert_select "input[name='oauth_app[always_available]'][disabled]"
    assert_select "input[name='oauth_app[shared]'][disabled]"
  end

  test "admin preset registers the deployment tenant without accepting a URL from the UI" do
    @app.destroy!
    login(@bob)
    get console_integrations_url
    assert_select "input[name=resource_url]", count: 0
    calls = []
    transport = ->(**request) do
      calls << request
      assert_equal "https://runlayer.example.test/api/v1/oauth/register", request[:url]
      body = JSON.parse(request[:body])
      assert_equal "client_secret_post", body["token_endpoint_auth_method"]
      assert_equal "mcp:proxy", body["scope"]
      HttpClient::Response.new(status: 201, body: { client_id: "registered-client", client_secret: "synthetic-client-secret" }.to_json)
    end
    Console::OauthAppsController.registration_client_factory = -> { Oauth::DynamicRegistration.new(http: HttpClient.new(http: transport)) }
    with_env("RUNLAYER_MCP_URL" => nil) do
      assert_no_difference "OauthApp.count" do
        post console_oauth_app_preset_path(provider: "runlayer")
      end
    end
    assert_empty calls
    assert_difference "OauthApp.count", 1 do
      post console_oauth_app_preset_path(provider: "runlayer"), params: { resource_url: "https://ignored.example.test/mcp" }
    end
    @app = OauthApp.find_by!(slug: "runlayer")
    assert_empty @app.labels
    assert @app.always_available?
    refute @app.shared?
    assert_redirected_to oauth_start_path(slug: "runlayer")
    assert_no_difference "OauthApp.count" do
      post console_oauth_app_preset_path(provider: "runlayer")
    end
    assert_equal 1, calls.length
    assert_equal RESOURCE, @app.reload.provider_strategy.resource_url
  end

  private

  def login(user)
    delete logout_url
    post login_url, params: { email: user.email, password: "password123456" }
  end

  def start_flow
    get oauth_start_url(slug: @app.slug)
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
      HttpClient::Response.new(status: 200, body: { access_token: access_token,
        refresh_token: "synthetic-refresh", expires_in: 3600, scope: "mcp:proxy" }.to_json)
    end
    Oauth::FlowsController.exchange_client_factory = -> { Broker::AuthorizationCodeClient.new(http: transport) }
    get oauth_callback_url(slug: @app.slug), params: { state: state, code: "synthetic-code" }
    assert_redirected_to console_integrations_path
    BrokerCredential.find_by!(oauth_app: @app, created_by: user)
  end

  def runlayer_entries(principal = @alice_principal)
    PrincipalSyncConfig.config_for(principal.reload).fetch("secrets").select do |entry|
      entry["rules"].any? { |rule| rule["host"] == "runlayer.example.test" }
    end
  end
end
