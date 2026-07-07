require "test_helper"
require "base64"
require "digest"
require "uri"

module Mcp
  class OauthControllerTest < ActionDispatch::IntegrationTest
    setup do
      @operator = users(:acme_admin)
      @saved_env = {
        "CENTAUR_JWT_SIGNING_SECRET" => ENV["CENTAUR_JWT_SIGNING_SECRET"],
        "CENTAUR_MCP_PUBLIC_URL" => ENV["CENTAUR_MCP_PUBLIC_URL"],
        "CENTAUR_CONSOLE_PUBLIC_URL" => ENV["CENTAUR_CONSOLE_PUBLIC_URL"]
      }
      ENV["CENTAUR_JWT_SIGNING_SECRET"] = "test-secret"
      ENV["CENTAUR_MCP_PUBLIC_URL"] = "http://localhost:3000/mcp"
      ENV["CENTAUR_CONSOLE_PUBLIC_URL"] = "http://www.example.com"
    end

    teardown do
      @saved_env.each do |key, value|
        if value.nil?
          ENV.delete(key)
        else
          ENV[key] = value
        end
      end
    end

    test "metadata advertises MCP OAuth endpoints" do
      get "/.well-known/oauth-authorization-server"

      assert_response :ok
      body = JSON.parse(response.body)
      assert_equal "http://www.example.com", body.fetch("issuer")
      assert_equal "http://www.example.com/mcp/oauth/authorize", body.fetch("authorization_endpoint")
      assert_equal "http://www.example.com/mcp/oauth/token", body.fetch("token_endpoint")
      assert_equal "http://www.example.com/mcp/oauth/register", body.fetch("registration_endpoint")
      assert_includes body.fetch("code_challenge_methods_supported"), "S256"
    end

    test "dynamic client registration creates a public PKCE client" do
      assert_difference -> { McpOauthClient.count }, 1 do
        post "/mcp/oauth/register",
             params: {
               client_name: "Amp",
               redirect_uris: [ "http://127.0.0.1:49152/callback" ],
               scope: "mcp:tools"
             },
             as: :json
      end

      assert_response :created
      body = JSON.parse(response.body)
      assert_match(/\Amoc_/, body.fetch("client_id"))
      assert_equal "none", body.fetch("token_endpoint_auth_method")
      assert_equal "mcp:tools", body.fetch("scope")
    end

    test "dynamic client registration rejects non-loopback redirect URIs" do
      assert_no_difference -> { McpOauthClient.count } do
        post "/mcp/oauth/register",
             params: {
               client_name: "Attacker",
               redirect_uris: [ "https://evil.example/callback" ],
               scope: "mcp:tools"
             },
             as: :json
      end

      assert_response :bad_request
      assert_equal "invalid_client_metadata", JSON.parse(response.body).fetch("error")
    end

    test "authorize rejects non-loopback redirect URIs even when already stored" do
      client = create_client
      client.update_column(:redirect_uris, [ "https://evil.example/callback" ])
      post login_url, params: { email: @operator.email, password: "password123456" }

      assert_no_difference -> { McpOauthAuthorizationCode.count } do
        get "/mcp/oauth/authorize",
            params: authorize_params(client).merge(redirect_uri: "https://evil.example/callback")
      end

      assert_response :bad_request
      assert_includes response.body, "redirect_uri is not registered"
    end

    test "authorize redirects signed-out users through login and preserves the request" do
      client = create_client
      get "/mcp/oauth/authorize", params: authorize_params(client)

      assert_redirected_to login_path

      post login_url, params: { email: @operator.email, password: "password123456" }
      assert_match %r{\Ahttp://www.example.com/mcp/oauth/authorize\?}, response.location
    end

    test "authorize accepts dynamic loopback redirect ports" do
      client = create_client(redirect_uris: [ "http://localhost/callback" ])
      approval_params = authorize_params(client).merge(
        redirect_uri: "http://localhost:49153/callback"
      )
      post login_url, params: { email: @operator.email, password: "password123456" }

      assert_no_difference -> { McpOauthAuthorizationCode.count } do
        get "/mcp/oauth/authorize", params: approval_params
      end

      assert_response :ok
      assert_select "form[action=?]", "/mcp/oauth/authorize"

      post "/mcp/oauth/authorize", params: approval_params.merge(decision: "approve")
      assert_response :redirect
      redirect = URI.parse(response.location)
      assert_equal "localhost", redirect.host
      assert_equal 49153, redirect.port
      assert Rack::Utils.parse_nested_query(redirect.query).key?("code")
    end

    test "authorization approval denial redirects without issuing a code" do
      client = create_client
      post login_url, params: { email: @operator.email, password: "password123456" }

      assert_no_difference -> { McpOauthAuthorizationCode.count } do
        post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "deny")
      end

      assert_response :redirect
      redirect = URI.parse(response.location)
      query = Rack::Utils.parse_nested_query(redirect.query)
      assert_equal "access_denied", query.fetch("error")
      assert_equal "state-test", query.fetch("state")
    end

    test "authorization code exchange returns a JWT access token for the console principal" do
      client = create_client
      code = authorize_code(client)
      stored_code = McpOauthAuthorizationCode.find_usable(code)
      assert_equal @operator, stored_code.user
      assert_equal "http://localhost:3000/mcp", stored_code.resource
      assert_match(/\Aprn_/, stored_code.principal.oid)

      exchange_authorization_code(client, code)

      assert_response :ok
      body = JSON.parse(response.body)
      assert_equal "Bearer", body.fetch("token_type")
      assert_equal "mcp:tools", body.fetch("scope")
      assert_match(/\Amcprt_/, body.fetch("refresh_token"))

      jwt_payload = decode_jwt_payload(body.fetch("access_token"))
      assert_equal "http://www.example.com", jwt_payload.fetch("iss")
      assert_equal "http://localhost:3000/mcp", jwt_payload.fetch("aud")
      assert_equal stored_code.principal.oid, jwt_payload.fetch("principal_id")
      assert_equal @operator.email, jwt_payload.fetch("email")
      assert_equal "mcp:tools", jwt_payload.fetch("scope")
    end

    test "authorization approval seeds new console principals with the user-mcp role" do
      client = create_client

      code = authorize_code(client)

      principal = McpOauthAuthorizationCode.find_usable(code).principal
      role = Role.find_by(namespace: principal.namespace, foreign_id: "user-mcp")
      assert role, "expected the user-mcp role to be created"
      assert_equal "User MCP", role.name
      assert_equal "centaur", role.labels["managed-by"]
      assert_includes principal.roles, role
    end

    test "authorization approval reuses an existing user-mcp role" do
      existing = Role.create!(
        namespace: "default",
        foreign_id: "user-mcp",
        name: "Custom user role",
        created_by: @operator
      )
      client = create_client

      assert_no_difference -> { Role.count } do
        code = authorize_code(client)
        principal = McpOauthAuthorizationCode.find_usable(code).principal
        assert_includes principal.roles, existing
      end
    end

    test "authorization approval does not restore a removed user-mcp role on existing principals" do
      client = create_client
      code = authorize_code(client)
      principal = McpOauthAuthorizationCode.find_usable(code).principal
      principal.principal_roles.destroy_all

      post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve")

      assert_response :redirect
      assert_empty principal.reload.roles
    end

    test "authorization code exchange rejects users disabled after consent" do
      client = create_client
      code = authorize_code(client)
      stored_code = McpOauthAuthorizationCode.find_usable(code)
      @operator.update!(status: :disabled)

      assert_no_difference -> { McpOauthRefreshToken.count } do
        exchange_authorization_code(client, code)
      end

      assert_response :bad_request
      assert_equal "invalid_grant", JSON.parse(response.body).fetch("error")
      assert stored_code.reload.consumed_at.present?
    end

    test "refresh token exchange rejects inactive users and revokes their tokens" do
      client = create_client
      code = authorize_code(client)
      exchange_authorization_code(client, code)
      refresh_token = JSON.parse(response.body).fetch("refresh_token")
      issued = McpOauthRefreshToken.find_usable(refresh_token)
      extra = McpOauthRefreshToken.create!(
        mcp_oauth_client: client,
        user: @operator,
        principal: issued.principal,
        resource: issued.resource,
        scopes: issued.scopes
      )
      @operator.update_column(:status, "disabled")

      post "/mcp/oauth/token",
           params: {
             grant_type: "refresh_token",
             client_id: client.public_client_id,
             refresh_token: refresh_token
           }

      assert_response :bad_request
      assert_equal "invalid_grant", JSON.parse(response.body).fetch("error")
      assert issued.reload.revoked_at.present?
      assert extra.reload.revoked_at.present?
      assert_equal 0, @operator.mcp_oauth_refresh_tokens.usable.count
    end

    private

    def create_client(redirect_uris: [ redirect_uri ])
      McpOauthClient.create!(
        name: "Amp",
        redirect_uris: redirect_uris,
        grant_types: McpOauthClient::DEFAULT_GRANT_TYPES,
        response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
        scopes: McpOauthClient::DEFAULT_SCOPES
      )
    end

    def authorize_params(client)
      {
        response_type: "code",
        client_id: client.public_client_id,
        redirect_uri: redirect_uri,
        scope: "mcp:tools",
        state: "state-test",
        resource: "http://localhost:3000/mcp",
        code_challenge: code_challenge,
        code_challenge_method: "S256"
      }
    end

    def authorize_code(client)
      approval_params = authorize_params(client)
      post login_url, params: { email: @operator.email, password: "password123456" }

      assert_no_difference -> { McpOauthAuthorizationCode.count } do
        get "/mcp/oauth/authorize", params: approval_params
      end
      assert_response :ok
      assert_select "form[action=?]", "/mcp/oauth/authorize"

      post "/mcp/oauth/authorize", params: approval_params.merge(decision: "approve")
      assert_response :redirect
      redirect = URI.parse(response.location)
      Rack::Utils.parse_nested_query(redirect.query).fetch("code")
    end

    def exchange_authorization_code(client, code)
      post "/mcp/oauth/token",
           params: {
             grant_type: "authorization_code",
             client_id: client.public_client_id,
             code: code,
             redirect_uri: redirect_uri,
             code_verifier: code_verifier
           }
    end

    def redirect_uri = "http://127.0.0.1:49152/callback"

    def code_verifier = "test-code-verifier"

    def code_challenge
      Base64.urlsafe_encode64(Digest::SHA256.digest(code_verifier), padding: false)
    end

    def decode_jwt_payload(token)
      _header, payload, _signature = token.split(".")
      JSON.parse(Base64.urlsafe_decode64(payload))
    end
  end
end
