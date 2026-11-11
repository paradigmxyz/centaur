require "test_helper"
require "base64"
require "digest"
require "uri"

module Api
  module V1
    # The SPA MCP consent page's server side: GET /api/v1/mcp/authorization
    # (ticket details) and the decision form post, POST /mcp/oauth/consent
    # (303 to the client). The authorize entry point and token exchange are
    # covered by test/controllers/mcp/oauth_controller_test.rb.
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

      # The consent form's post. token: false omits the authenticity token.
      def decide(ticket, decision, token: nil, headers: {}, **extra)
        @console_csrf_token = nil
        token = console_csrf_token if token.nil?
        params = { request: ticket, decision: decision, **extra }
        params[:authenticity_token] = token if token
        post "/mcp/oauth/consent", params: params, headers: headers
      end

      def redirect_query
        Rack::Utils.parse_nested_query(URI.parse(response.location).query)
      end

      def consent_error
        uri = URI.parse(response.location)
        assert_equal "/mcp/oauth/consent", uri.path
        Rack::Utils.parse_nested_query(uri.query).fetch("error")
      end

      test "the details endpoint refuses anonymous, disabled, and pending callers" do
        assert_active_user_api :get, api_v1_mcp_authorization_path(request: "x")
      end

      test "bearer keys cannot read a consent" do
        get api_v1_mcp_authorization_path(request: "x"), headers: bearer_headers
        assert_response :forbidden
        assert_equal "session_required", json_body.dig("error", "details", "reason")
      end

      test "the decision form needs a signed-in active session" do
        post "/mcp/oauth/consent", params: { request: "x", decision: "approve" }
        assert_redirected_to login_path

        sign_in_console(users(:pending_user))
        decide("x", "approve")
        assert_redirected_to pending_path

        user = create_console_user
        sign_in_console(user)
        ticket = start_consent
        user.update!(status: :disabled)
        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          post "/mcp/oauth/consent", params: { request: ticket, decision: "approve" }
        end
        assert_redirected_to login_path
        assert_nil session[:user_id]
      end

      test "details show what is being authorized and where the decision goes" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent

        get api_v1_mcp_authorization_path, params: { request: ticket }
        assert_response :ok
        assert_equal "no-store", response.headers["Cache-Control"]
        data = json_body["data"]
        assert_equal(
          {
            "client" => { "id" => @client.public_client_id, "name" => "Amp" },
            "redirect_host" => "127.0.0.1",
            "resource" => "http://localhost:3000/mcp",
            "scopes" => [ "mcp:tools" ],
            "user" => { "email" => users(:acme_admin).email },
            "decision_path" => "/mcp/oauth/consent"
          },
          data.except("csrf_token")
        )
        assert data["csrf_token"].present?
        assert_not_includes response.body, "code_challenge"
        assert_not_includes response.body, REDIRECT_URI
      end

      test "a regular user approves for themselves and the code goes only to the registered URI" do
        member = users(:member_user)
        sign_in_console(member)
        ticket = start_consent

        assert_difference -> { McpOauthAuthorizationCode.count }, 1 do
          decide(ticket, "approve", redirect_uri: "https://evil.example/steal")
        end
        assert_response :see_other
        assert response.location.start_with?("#{REDIRECT_URI}?"), response.location
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
        assert_response :see_other
        assert_equal "access_denied", redirect_query.fetch("error")
        assert_nil redirect_query["code"]
      end

      test "a decision cannot be replayed" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        decide(ticket, "approve")
        assert_response :see_other

        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          decide(ticket, "approve")
        end
        assert_equal "invalid_consent_request", consent_error

        get api_v1_mcp_authorization_path, params: { request: ticket }
        assert_response :not_found
      end

      test "replaying the original session cookie cannot decide a ticket twice" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        token = console_csrf_token
        session_key = Rails.application.config.session_options.fetch(:key)
        original_cookie = cookies[session_key]

        decide(ticket, "approve", token: token)
        assert_response :see_other
        assert redirect_query["code"].present?

        # The cookie from before the decision still lists the nonce as pending.
        cookies[session_key] = original_cookie
        get api_v1_me_path
        assert_equal 1, Array(session[:mcp_consent_nonces]).size, "the replayed cookie should still list the nonce"
        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          decide(ticket, "approve", token: token)
        end
        assert_equal "invalid_consent_request", consent_error
        assert_equal 1, McpOauthConsent.where.not(consumed_at: nil).count
      end

      test "concurrent decisions consume a ticket exactly once" do
        user = users(:acme_admin)
        session = {}
        request = Mcp::AuthorizationRequest.from_params(authorize_params)
        ticket = Mcp::ConsentTicket.issue(session, user: user, request: request)

        # Each thread starts from its own copy of the same session, as racing
        # requests carrying the same cookie would.
        start = Concurrent::CountDownLatch.new(1)
        threads = Array.new(4) do
          Thread.new do
            copy = session.deep_dup
            start.wait
            Mcp::ConsentTicket.consume(copy, ticket, user: user).present?
          end
        end
        start.count_down
        assert_equal [ false, false, false, true ], threads.map(&:value).sort_by { |won| won ? 1 : 0 }
      end

      test "an unknown decision is rejected without consuming the ticket" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          decide(ticket, "maybe")
        end
        assert_equal "invalid_decision", consent_error

        decide(ticket, "approve")
        assert_response :see_other
        assert redirect_query["code"].present?
      end

      test "decisions require the CSRF token and a same-origin request" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          decide(ticket, "approve", token: false)
          assert_equal "csrf", consent_error

          decide(ticket, "approve", token: "not-a-token")
          assert_equal "csrf", consent_error

          decide(ticket, "approve", headers: { "Origin" => "https://evil.example" })
          assert_equal "csrf", consent_error
        end

        decide(ticket, "approve")
        assert redirect_query["code"].present?
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
        assert_equal "invalid_consent_request", consent_error
      end

      test "a ticket is bound to the browser session that started it" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent

        other = open_session
        other.extend(ConsoleApiAuthHelper)
        other.sign_in_console(users(:acme_admin))
        other.get api_v1_mcp_authorization_path, params: { request: ticket }
        assert_equal 404, other.response.status
        other.post "/mcp/oauth/consent", params: { request: ticket, decision: "approve",
                                                   authenticity_token: other.console_csrf_token }
        assert_equal 0, McpOauthAuthorizationCode.count
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
          assert_equal "invalid_consent_request", consent_error
        end
        assert_equal 0, McpOauthAuthorizationCode.count
      end

      test "the request is revalidated at decision time" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        @client.update_column(:redirect_uris, [ "http://127.0.0.1:49152/other" ])

        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          decide(ticket, "approve")
        end
        assert_equal "invalid_authorization_request", consent_error
      end

      test "a registration switched to a javascript redirect URI after consent started is refused" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        @client.update_column(:redirect_uris, [ REDIRECT_URI, "javascript://x/%0aalert(document.domain)//" ])
        # Even a ticket whose stored redirect_uri is the javascript one is refused.
        forged_request = Mcp::AuthorizationRequest.new(
          authorize_params.stringify_keys.transform_values(&:to_s).merge("redirect_uri" => "javascript://x/%0aalert(document.domain)//")
        )
        assert_not forged_request.valid?
        assert_nil forged_request.error_redirect_url

        decide(ticket, "approve")
        assert response.location.start_with?("#{REDIRECT_URI}?")
      end

      test "details refuse a request whose client has since been deleted" do
        sign_in_console(users(:acme_admin))
        ticket = start_consent
        @client.destroy!

        get api_v1_mcp_authorization_path, params: { request: ticket }
        assert_response :unprocessable_entity
        assert_equal "Unknown client.", json_body.dig("error", "message")
        assert_nil json_body.dig("error", "details", "redirect_to")

        decide(ticket, "approve")
        assert_equal "invalid_authorization_request", consent_error
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
