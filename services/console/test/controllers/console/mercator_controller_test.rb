require "test_helper"

class Console::MercatorControllerTest < ActionDispatch::IntegrationTest
  WALLET = "0x#{'12' * 20}".freeze

  setup do
    @user = users(:acme_admin)
    sign_in(@user)
    @app = OauthApp.create!(slug: "mercator", provider: "mercator", client_id: "test-client",
      client_secret: "test-secret", allowed_scopes: [ "mercator:tools" ], created_by: @user)
    @exchange_count = 0
    Oauth::FlowsController.exchange_client_factory = -> {
      Broker::AuthorizationCodeClient.new(http: ->(**request) {
        @exchange_count += 1
        assert_equal "https://mercator.sh/oauth/token", request[:url]
        assert request[:form]["code_verifier"].present?
        HttpClient::Response.new(status: 200, body: {
          access_token: "synthetic-access", refresh_token: "synthetic-refresh", expires_in: 3600, scope: "mercator:tools"
        }.to_json)
      })
    }
    @wallet = WALLET
    Oauth::FlowsController.identity_http_client_factory = -> {
      HttpClient.new(http: ->(**request) {
        assert_equal "https://mercator.sh/mcp/auth", request[:url]
        HttpClient::Response.new(status: 200, body: {
          jsonrpc: "2.0", id: 1, result: { structuredContent: { oauthAuthenticated: true, account: { walletAddress: @wallet } } }
        }.to_json)
      })
    }
  end

  teardown do
    Oauth::FlowsController.exchange_client_factory = -> { Broker::AuthorizationCodeClient.new }
    Oauth::FlowsController.identity_http_client_factory = -> { HttpClient.new }
    Console::MercatorController.connection_client_factory = -> { Mercator::ConnectionClient.new }
  end

  test "admin sees setup and ordinary users cannot view or start any Mercator flow" do
    get console_mercator_path
    assert_response :ok
    assert_select "button", text: "Connect"
    get console_integrations_path
    assert_response :ok
    assert_select "button", text: "Connect"
    sign_in(users(:member_user))
    get console_integrations_path
    assert_response :ok
    assert_select "#organization-integrations", count: 0
    get console_mercator_path
    assert_redirected_to console_integrations_path
    post connect_console_mercator_path
    assert_redirected_to console_integrations_path
    get oauth_start_path(slug: "mercator")
    assert_redirected_to console_integrations_path
    get oauth_callback_path(slug: "mercator"), params: { code: "x", state: "x" }
    assert_redirected_to console_integrations_path
    get console_integrations_path
    assert_no_match "Mercator", response.body
    assert_equal 0, @exchange_count
  end

  test "disabled admin cannot connect" do
    @user.update!(status: :disabled)
    post connect_console_mercator_path
    assert_response :redirect
    assert_equal 0, @exchange_count
  end

  test "consent stores the credential without granting agent access" do
    state = start_flow
    assert_difference("BrokerCredential.count", 1) { finish_flow(state) }
    assert_redirected_to console_mercator_path
    credential = Mercator::Connection.credential
    assert_equal WALLET, credential.provider_subject
    secret = credential.static_secret
    assert_equal "token_broker", secret.source.source_type
    assert_equal [ { "host" => "mercator.sh", "methods" => [ "POST" ], "paths" => [ "/mcp/auth" ] } ], secret.rules.map(&:to_proxy_rule)
    assert_empty Grant.where(static_secret: secret)
    assert_not Role.exists?(foreign_id: "mercator-agents")
    principal = Principal.create!(name: "New agent", created_by: @user)
    assert_not_includes principal.connected_tool_names, "mercator"
    assert_no_match "synthetic-", response.body
  end

  test "callback is one use and reconnect preserves secret and operator role choices" do
    state = start_flow
    finish_flow(state)
    credential = Mercator::Connection.credential
    secret_id = credential.static_secret.id
    role = Role.first!
    post console_secret_grant_role_path("static", credential.static_secret.oid), params: { role_id: role.oid }
    assert role.grants.exists?(static_secret: credential.static_secret)
    membership_ids = role.principal_roles.ids.sort
    default_assignment = role.assign_by_default?
    finish_flow(state)
    assert_response :bad_request
    assert_equal 1, @exchange_count
    state = start_flow
    assert_no_difference("BrokerCredential.count") { finish_flow(state) }
    assert_equal secret_id, credential.reload.static_secret.id
    assert_equal default_assignment, role.reload.assign_by_default?
    assert_equal membership_ids, role.principal_roles.ids.sort
    assert role.grants.exists?(static_secret: credential.static_secret)
  end

  test "reconnect cannot silently replace the wallet" do
    finish_flow(start_flow)
    @wallet = "0x#{'34' * 20}"
    assert_no_difference("BrokerCredential.count") { finish_flow(start_flow) }
    assert_response :unprocessable_entity
    assert_equal WALLET, Mercator::Connection.credential.provider_subject
  end

  test "another signed in admin cannot finish the first admin's flow" do
    state = start_flow
    sign_in(users(:globex_admin))
    finish_flow(state)
    assert_response :bad_request
    assert_equal 0, @exchange_count
  end

  test "cancel and invalid state create no credentials" do
    state = start_flow
    get oauth_callback_path(slug: "mercator"), params: { state: state, error: "access_denied" }
    assert_response :unprocessable_entity
    finish_flow("invalid")
    assert_response :bad_request
    assert_equal 0, @exchange_count
    assert_nil Mercator::Connection.credential
  end

  test "invalid wallet identity creates no credential or grant" do
    @wallet = "invalid"
    assert_no_difference("Grant.count") { finish_flow(start_flow) }
    assert_response :unprocessable_entity
    assert_nil Mercator::Connection.credential
  end

  test "registration is automatic and reused without asking for operator fields" do
    @app.destroy!
    client = Minitest::Mock.new
    client.expect(:register, { "client_id" => "registered", "client_secret" => "registration-secret" }, [ "http://www.example.com/oauth/mercator/callback" ])
    Console::MercatorController.connection_client_factory = -> { client }
    assert_difference("OauthApp.count", 1) { post connect_console_mercator_path }
    assert_redirected_to oauth_start_path(slug: "mercator")
    assert_no_difference("OauthApp.count") { post connect_console_mercator_path }
    client.verify
  end

  test "manual credentials block duplicate onboarding" do
    @app.destroy!
    BrokerCredential.create!(name: "Existing", token_endpoint: "https://mercator.sh/oauth/token", client_id: "manual")
    get console_mercator_path
    assert_select "a", text: "Review credentials"
    assert_select "button", text: "Connect", count: 0
    assert_no_difference("OauthApp.count") { post connect_console_mercator_path }
    assert_redirected_to console_mercator_path
  end

  test "status failure is not displayed as a zero balance" do
    finish_flow(start_flow)
    client = Object.new
    def client.status(*) = raise Broker::ExchangeError.new("unavailable", stage: "network")
    Console::MercatorController.connection_client_factory = -> { client }
    get console_mercator_path
    assert_response :ok
    assert_match "Balance is currently unavailable", response.body
    assert_no_match "synthetic-", response.body
  end

  test "refresh keeps the Mercator client binding and updates the proxy source" do
    finish_flow(start_flow)
    credential = Mercator::Connection.credential
    credential.update!(next_attempt_at: 1.minute.ago)
    credential.refresh_client = Broker::RefreshClient.new(http: ->(**request) {
      assert_equal "https://mercator.sh/oauth/token", request[:url]
      assert_equal "refresh_token", request[:form]["grant_type"]
      assert_equal @app.client_id, request[:form]["client_id"]
      HttpClient::Response.new(status: 200, body: { access_token: "rotated-synthetic", refresh_token: "rotated-refresh", expires_in: 3600 }.to_json)
    })
    credential.refresh!
    assert_not credential.dead?
    assert_equal "rotated-synthetic", credential.static_secret.source.to_proxy_source["value"]
  end

  test "expired consent cannot mint a credential" do
    state = start_flow
    travel 11.minutes do
      finish_flow(state)
      assert_response :bad_request
    end
    assert_equal 0, @exchange_count
  end

  private

  def sign_in(user)
    post login_path, params: { email: user.email, password: "password123456" }
  end

  def start_flow
    get oauth_start_path(slug: "mercator")
    assert_response :redirect
    uri = URI.parse(response.location)
    assert_equal "mercator.sh", uri.host
    query = URI.decode_www_form(uri.query).to_h
    assert_equal "S256", query["code_challenge_method"]
    assert_equal "mercator:tools", query["scope"]
    query.fetch("state")
  end

  def finish_flow(state)
    get oauth_callback_path(slug: "mercator"), params: { state: state, code: "synthetic-code" }
  end
end
