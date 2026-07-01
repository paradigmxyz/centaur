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

    test "authorize redirects signed-out users through login and preserves the request" do
      client = create_client
      get "/mcp/oauth/authorize", params: authorize_params(client)

      assert_redirected_to login_path

      post login_url, params: { email: @operator.email, password: "password123456" }
      assert_match %r{\Ahttp://www.example.com/mcp/oauth/authorize\?}, response.location
    end

    test "authorize accepts dynamic loopback redirect ports" do
      client = create_client(redirect_uris: [ "http://localhost/callback" ])
      post login_url, params: { email: @operator.email, password: "password123456" }

      get "/mcp/oauth/authorize",
          params: authorize_params(client).merge(
            redirect_uri: "http://localhost:49153/callback"
          )

      assert_response :redirect
      redirect = URI.parse(response.location)
      assert_equal "localhost", redirect.host
      assert_equal 49153, redirect.port
      assert Rack::Utils.parse_nested_query(redirect.query).key?("code")
    end

    test "authorization code exchange returns a JWT access token for the console principal" do
      client = create_client
      post login_url, params: { email: @operator.email, password: "password123456" }

      get "/mcp/oauth/authorize", params: authorize_params(client)
      assert_response :redirect
      redirect = URI.parse(response.location)
      code = Rack::Utils.parse_nested_query(redirect.query).fetch("code")
      stored_code = McpOauthAuthorizationCode.find_usable(code)
      assert_equal @operator, stored_code.user
      assert_equal "http://localhost:3000/mcp", stored_code.resource
      assert_match(/\Aprn_/, stored_code.principal.oid)

      post "/mcp/oauth/token",
           params: {
             grant_type: "authorization_code",
             client_id: client.public_client_id,
             code: code,
             redirect_uri: redirect_uri,
             code_verifier: code_verifier
           }

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
