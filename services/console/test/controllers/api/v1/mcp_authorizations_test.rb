require "test_helper"
require "base64"
require "digest"
require "uri"

module Api
  module V1
    # The SPA MCP consent page's endpoints: GET /api/v1/mcp/authorization
    # (ticket details) and POST /api/v1/mcp/authorization/decision. The
    # authorize entry point and token exchange are covered by
    # test/controllers/mcp/oauth_controller_test.rb.
    class McpAuthorizationsTest < ActionDispatch::IntegrationTest
      REDIRECT_URI = "http://127.0.0.1:49152/callback".freeze

      setup do
        @saved_env = ENV.to_hash.slice("CENTAUR_MCP_PUBLIC_URL")
        ENV["CENTAUR_MCP_PUBLIC_URL"] = "http://localhost:3000/mcp"
        @client = McpOauthClient.create!(
          name: "Amp",
          redirect_uris: [ REDIRECT_URI ],
          grant_types: McpOauthClient::DEFAULT_GRANT_TYPES,
          response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
          scopes: McpOauthClient::DEFAULT_SCOPES
        )
      end

      teardown do
        ENV.delete("CENTAUR_MCP_PUBLIC_URL")
        @saved_env.each { |key, value| ENV[key] = value }
      end

      def authorize_params(**overrides)
        {
          response_type: "code",
          client_id: @client.public_client_id,
          redirect_uri: REDIRECT_URI,
          scope: "mcp:tools",
          state: "state-test",
          resource: "http://localhost:3000/mcp",
          code_challenge: Base64.urlsafe_encode64(Digest::SHA256.digest("verifier"), padding: false),
          code_challenge_method: "S256"
        }.merge(overrides)
      end

      def start_consent(**overrides)
        get "/mcp/oauth/authorize", params: authorize_params(**overrides)
        assert_response :redirect
        uri = URI.parse(response.location)
        assert_equal "/mcp/oauth/consent", uri.path
        Rack::Utils.parse_nested_query(uri.query).fetch("request")
      end

      def decide(ticket, decision, headers: nil, **extra)
        @console_csrf_token = nil
        headers ||= csrf_headers
        post api_v1_mcp_authorization_decision_path,
             params: { data: { request: ticket, decision: decision, **extra } }, headers: headers, as: :json
      end

      def redirect_query
        Rack::Utils.parse_nested_query(URI.parse(json_body.dig("data", "redirect_to")).query)
      end

      test "both endpoints refuse anonymous, disabled, pending, and CSRF-less callers" do
        assert_active_user_api :get, api_v1_mcp_authorization_path(request: "x")
        assert_active_user_api :post, api_v1_mcp_authorization_decision_path,
                               params: { data: { request: "x", decision: "approve" } }
      end

      test "bearer keys cannot read or decide a consent" do
        get api_v1_mcp_authorization_path(request: "x"), headers: bearer_headers
        assert_response :forbidden
        assert_equal "session_required", json_body.dig("error", "details", "reason")

        post api_v1_mcp_authorization_decision_path, params: { data: { request: "x", decision: "approve" } },
             headers: bearer_headers, as: :json
        assert_response :forbidden
        assert_equal "session_required", json_body.dig("error", "details", "reason")
      end

      test "details show what is being authorized" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent

        get api_v1_mcp_authorization_path, params: { request: ticket }
        assert_response :ok
        assert_equal(
          {
            "client" => { "id" => @client.public_client_id, "name" => "Amp" },
            "redirect_host" => "127.0.0.1",
            "resource" => "http://localhost:3000/mcp",
            "scopes" => [ "mcp:tools" ],
            "user" => { "email" => users(:acme_admin).email }
          },
          json_body["data"]
        )
        assert_not_includes response.body, "code_challenge"
      end

      test "a regular user approves for themselves and the code redirects only to the registered URI" do
        member = users(:member_user)
        sign_in_console(member)
        ticket = start_consent

        assert_difference -> { McpOauthAuthorizationCode.count }, 1 do
          decide(ticket, "approve", redirect_uri: "https://evil.example/steal")
        end
        assert_response :ok
        assert_equal "approve", json_body.dig("data", "decision")
        redirect_to = json_body.dig("data", "redirect_to")
        assert redirect_to.start_with?("#{REDIRECT_URI}?"), redirect_to
        assert_equal "state-test", redirect_query.fetch("state")

        code = McpOauthAuthorizationCode.find_usable(redirect_query.fetch("code"))
        assert_equal member, code.user
        assert_equal REDIRECT_URI, code.redirect_uri
        assert_equal [ "mcp:tools" ], code.scopes
        assert @client.reload.last_used_at.present?
      end

      test "deny never issues a code" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          decide(ticket, "deny")
        end
        assert_response :ok
        assert_equal "access_denied", redirect_query.fetch("error")
        assert_nil redirect_query["code"]
      end

      test "a decision cannot be replayed" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        decide(ticket, "approve")
        assert_response :ok

        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          decide(ticket, "approve")
        end
        assert_response :not_found
        assert_equal "invalid_consent_request", json_body.dig("error", "details", "reason")

        get api_v1_mcp_authorization_path, params: { request: ticket }
        assert_response :not_found
      end

      test "an unknown decision is rejected without consuming the ticket" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          decide(ticket, "maybe")
        end
        assert_response :unprocessable_entity

        decide(ticket, "approve")
        assert_response :ok
      end

      test "decisions require the CSRF token and a same-origin request" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          decide(ticket, "approve", headers: {})
          assert_response :forbidden
          assert_equal "csrf", json_body.dig("error", "details", "reason")

          decide(ticket, "approve", headers: csrf_headers.merge("Origin" => "https://evil.example"))
          assert_response :forbidden
        end

        decide(ticket, "approve")
        assert_response :ok
      end

      test "a ticket is bound to the user it was issued to" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent

        sign_in_console(users(:member_user))
        get api_v1_mcp_authorization_path, params: { request: ticket }
        assert_response :not_found
        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          decide(ticket, "approve")
        end
        assert_response :not_found
      end

      test "a ticket is bound to the browser session that started it" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent

        other = open_session
        other.extend(ConsoleApiAuthHelper)
        other.sign_in_console(users(:acme_admin))
        other.get api_v1_mcp_authorization_path, params: { request: ticket }
        assert_equal 404, other.response.status
      end

      test "tampered and expired tickets are rejected" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent

        get api_v1_mcp_authorization_path, params: { request: "#{ticket}x" }
        assert_response :not_found
        forged = Rails.application.message_verifier(:other).generate({ "user" => users(:acme_admin).id })
        get api_v1_mcp_authorization_path, params: { request: forged }
        assert_response :not_found

        travel 11.minutes do
          get api_v1_mcp_authorization_path, params: { request: ticket }
          assert_response :not_found
          decide(ticket, "approve")
          assert_response :not_found
        end
      end

      test "the request is revalidated at decision time" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        @client.update_column(:redirect_uris, [ "http://127.0.0.1:49152/other" ])

        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          decide(ticket, "approve")
        end
        assert_response :unprocessable_entity
        assert_equal "invalid_authorization_request", json_body.dig("error", "details", "reason")
        assert_nil json_body.dig("error", "details", "redirect_to")
      end

      test "details refuse a request whose client has since been deleted" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        @client.destroy!

        get api_v1_mcp_authorization_path, params: { request: ticket }
        assert_response :unprocessable_entity
        assert_equal "Unknown client.", json_body.dig("error", "message")
        assert_nil json_body.dig("error", "details", "redirect_to")
      end

      test "a session holds only a few pending consents" do
        sign_in_console(users(:acme_admin))
        first = start_consent
        Mcp::ConsentTicket::MAX_PENDING.times { start_consent }

        get api_v1_mcp_authorization_path, params: { request: first }
        assert_response :not_found
      end

      test "signing in again invalidates tickets from the old session" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        sign_in_console(users(:acme_admin))

        get api_v1_mcp_authorization_path, params: { request: ticket }
        assert_response :not_found
      end
    end
  end
end
