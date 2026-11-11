require "test_helper"
require "base64"
require "digest"
require "uri"

module Mcp
  class OauthControllerTest < ActionDispatch::IntegrationTest
    # The hidden fields origin/main's consent form posted (plus decision from
    # the button). Installed clients and open consent tabs depend on them.
    ORIGIN_MAIN_FORM_FIELDS = %w[
      authenticity_token response_type client_id redirect_uri scope state resource code_challenge code_challenge_method
    ].freeze

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

    test "dynamic client registration creates a hosted HTTPS client" do
      assert_difference -> { McpOauthClient.count }, 1 do
        post "/mcp/oauth/register",
             params: {
               client_name: "Claude",
               redirect_uris: [ "https://claude.ai/api/mcp/auth_callback" ],
               scope: "mcp:tools"
             },
             as: :json
      end

      assert_response :created
      body = JSON.parse(response.body)
      assert_equal [ "https://claude.ai/api/mcp/auth_callback" ], body.fetch("redirect_uris")
    end

    test "dynamic client registration creates a private-use scheme native client" do
      assert_difference -> { McpOauthClient.count }, 1 do
        post "/mcp/oauth/register",
             params: {
               client_name: "Cursor",
               redirect_uris: [ "cursor://anysphere.cursor-mcp/oauth/callback" ],
               scope: "mcp:tools"
             },
             as: :json
      end

      assert_response :created
      body = JSON.parse(response.body)
      assert_equal [ "cursor://anysphere.cursor-mcp/oauth/callback" ], body.fetch("redirect_uris")
    end

    test "dynamic client registration rejects non-loopback plain HTTP redirect URIs" do
      assert_no_difference -> { McpOauthClient.count } do
        post "/mcp/oauth/register",
             params: {
               client_name: "Attacker",
               redirect_uris: [ "http://evil.example/callback" ],
               scope: "mcp:tools"
             },
             as: :json
      end

      assert_response :bad_request
      assert_equal "invalid_client_metadata", JSON.parse(response.body).fetch("error")
    end

    test "authorize rejects non-loopback plain HTTP redirect URIs even when already stored" do
      client = create_client
      client.update_column(:redirect_uris, [ "http://evil.example/callback" ])
      console_password_login(@operator.email)

      assert_no_difference -> { McpOauthAuthorizationCode.count } do
        get "/mcp/oauth/authorize",
            params: authorize_params(client).merge(redirect_uri: "http://evil.example/callback")
      end

      assert_response :bad_request
      assert_includes response.body, "redirect_uri is not registered"
    end

    test "authorize redirects signed-out users through login and preserves the request" do
      client = create_client
      get "/mcp/oauth/authorize", params: authorize_params(client)

      assert_redirected_to login_path

      # The login form follows the remembered return path.
      console_password_login(@operator.email)
      assert_match %r{\A/mcp/oauth/authorize\?}, console_login_redirect
      assert_equal authorize_params(client).transform_keys(&:to_s),
                   Rack::Utils.parse_nested_query(URI.parse(console_login_redirect).query)
    end

    test "authorize accepts dynamic loopback redirect ports" do
      client = create_client(redirect_uris: [ "http://localhost/callback" ])
      approval_params = authorize_params(client).merge(
        redirect_uri: "http://localhost:49153/callback"
      )
      console_password_login(@operator.email)

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
      console_password_login(@operator.email)

      assert_no_difference -> { McpOauthAuthorizationCode.count } do
        post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "deny")
      end

      assert_response :redirect
      redirect = URI.parse(response.location)
      query = Rack::Utils.parse_nested_query(redirect.query)
      assert_equal "access_denied", query.fetch("error")
      assert_equal "state-test", query.fetch("state")
    end

    test "dynamic client registration rejects executable redirect URI schemes" do
      [
        "javascript://x/%0aalert(document.domain)//",
        "JavaScript://x/%0aalert(1)",
        "javascript:alert(1)",
        " javascript:alert(1)",
        "vbscript:msgbox(1)",
        "VBScript://x",
        "livescript:alert(1)",
        "data:text/html;base64,PHNjcmlwdD4=",
        "DATA:text/plain,x"
      ].each do |uri|
        assert_no_difference -> { McpOauthClient.count } do
          post "/mcp/oauth/register", params: { client_name: "Attacker", redirect_uris: [ uri ] }, as: :json
        end
        assert_response :bad_request, uri
        assert_equal "invalid_client_metadata", JSON.parse(response.body).fetch("error"), uri
      end
    end

    test "dynamic client registration still accepts native and extension custom-scheme callbacks" do
      [
        "vscode://vscode.github-authentication/did-authenticate",
        "chrome-extension://abcdefghijklmnopabcdefghijklmnop/callback",
        "com.example.app:/oauth2redirect"
      ].each do |uri|
        post "/mcp/oauth/register", params: { client_name: "Native", redirect_uris: [ uri ] }, as: :json
        assert_response :created, uri
        assert_equal [ uri ], JSON.parse(response.body).fetch("redirect_uris")
      end
    end

    test "authorize renders the consent page with the origin/main form fields" do
      client = create_client
      client.update!(name: "Amp <script>alert(1)</script>")
      console_password_login(@operator.email)

      assert_no_difference -> { McpOauthAuthorizationCode.count } do
        with_forgery_protection { get "/mcp/oauth/authorize", params: authorize_params(client) }
      end

      assert_response :ok
      assert_match %r{text/html}, response.media_type
      assert_select "title", "Authorize MCP Client · Centaur Console"
      assert_select "p", text: "Authorize MCP access."
      assert_select "h1", text: "Amp <script>alert(1)</script>"
      assert_not_includes response.body, "<script>alert(1)</script>"
      assert_select "p", text: "127.0.0.1"
      assert_select "dt", text: "Resource"
      assert_select "dd", text: "http://localhost:3000/mcp"
      assert_select "dd", text: "mcp:tools"
      assert_select "dd", text: @operator.email
      assert_select "form[action=?][method=post]", "/mcp/oauth/authorize" do
        assert_equal ORIGIN_MAIN_FORM_FIELDS.sort, css_select("input[type=hidden]").map { |input| input["name"] }.sort
        assert_select "input[name=client_id][value=?]", client.public_client_id
        assert_select "input[name=redirect_uri][value=?]", redirect_uri
        assert_select "input[name=state][value=?]", "state-test"
        assert_select "input[name=code_challenge][value=?]", code_challenge
        assert_select "button[type=submit][name=decision][value=deny]", text: "Deny"
        assert_select "button[type=submit][name=decision][value=approve]", text: "Allow"
      end
    end

    test "the consent page is standalone: plain CSS from public, no scripts" do
      client = create_client
      console_password_login(@operator.email)
      get "/mcp/oauth/authorize", params: authorize_params(client)

      assert_select "script", count: 0
      # A no-referrer policy makes browsers send "Origin: null" on the consent
      # post, which Rails' origin check rejects.
      assert_select "meta[name=referrer]", count: 0
      assert_select "link[rel=stylesheet]", count: 1 do |links|
        href = links.first["href"]
        assert_match %r{\A/oauth-page/oauth\.css\?v=\h{12}\z}, href
        assert File.file?(Rails.public_path.join(URI.parse(href).path.delete_prefix("/")))
      end
      assert_select "img[alt=Centaur]" do |images|
        assert File.file?(Rails.public_path.join(URI.parse(images.first["src"]).path.delete_prefix("/")))
      end
    end

    test "authorize returns trusted-client errors to the registered redirect_uri" do
      client = create_client
      console_password_login(@operator.email)

      {
        { scope: "admin" } => "invalid_scope",
        { response_type: "token" } => "unsupported_response_type",
        { code_challenge_method: "plain" } => "invalid_request",
        { code_challenge: "" } => "invalid_request",
        { resource: "https://other.example/mcp" } => "invalid_target"
      }.each do |overrides, error|
        get "/mcp/oauth/authorize", params: authorize_params(client).merge(overrides)
        assert_response :redirect
        redirect = URI.parse(response.location)
        assert_equal "127.0.0.1", redirect.host, overrides.inspect
        query = Rack::Utils.parse_nested_query(redirect.query)
        assert_equal error, query.fetch("error"), overrides.inspect
        assert_equal "state-test", query.fetch("state")
        assert_nil query["code"]
      end
    end

    test "authorize validates in origin/main order" do
      client = create_client
      console_password_login(@operator.email)

      # response_type is checked before redirect_uri; an untrusted redirect_uri
      # still means the error is shown, never redirected.
      get "/mcp/oauth/authorize", params: authorize_params(client).merge(response_type: "token", redirect_uri: "https://evil.example/cb")
      assert_response :bad_request
      assert_equal "Only response_type=code is supported.", response.body

      get "/mcp/oauth/authorize", params: authorize_params(client).merge(code_challenge_method: "plain", scope: "admin")
      assert_equal "code_challenge_method must be S256.", Rack::Utils.parse_nested_query(URI.parse(response.location).query)["error_description"]

      get "/mcp/oauth/authorize", params: authorize_params(client).merge(scope: "admin", resource: "https://other.example/mcp")
      assert_equal "invalid_scope", Rack::Utils.parse_nested_query(URI.parse(response.location).query)["error"]
    end

    test "authorize never redirects to an unknown client or unregistered redirect_uri" do
      client = create_client
      console_password_login(@operator.email)

      get "/mcp/oauth/authorize", params: authorize_params(client).merge(client_id: "moc_unknown")
      assert_response :bad_request
      assert_nil response.location
      assert_equal "Unknown client.", response.body

      get "/mcp/oauth/authorize", params: authorize_params(client).merge(redirect_uri: "https://evil.example/cb")
      assert_response :bad_request
      assert_nil response.location
      assert_includes response.body, "redirect_uri is not registered"
    end

    test "authorize fails closed without a configured MCP resource" do
      client = create_client
      console_password_login(@operator.email)
      with_env("CENTAUR_MCP_PUBLIC_URL" => nil, "CENTAUR_CONSOLE_MCP_PUBLIC_URL" => nil, "IRON_CONTROL_MCP_PUBLIC_URL" => nil) do
        get "/mcp/oauth/authorize", params: authorize_params(client)
      end
      assert_response :redirect
      assert_equal "invalid_target", Rack::Utils.parse_nested_query(URI.parse(response.location).query).fetch("error")
    end

    test "authorize sends pending users to the holding page and signs out disabled ones" do
      client = create_client
      console_password_login(users(:pending_user).email)
      get "/mcp/oauth/authorize", params: authorize_params(client)
      assert_redirected_to pending_path

      user = create_console_user
      sign_in_console(user)
      user.update!(status: :disabled)
      get "/mcp/oauth/authorize", params: authorize_params(client)
      assert_redirected_to login_path
      assert_equal "Your account is disabled.", flash[:alert]
      assert_nil session[:user_id]
    end

    test "approval requires an active console session" do
      client = create_client

      assert_no_difference -> { McpOauthAuthorizationCode.count } do
        post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve")
      end
      assert_redirected_to login_path

      console_password_login(users(:pending_user).email)
      assert_no_difference -> { McpOauthAuthorizationCode.count } do
        post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve")
      end
      assert_redirected_to pending_path

      user = create_console_user
      sign_in_console(user)
      user.update!(status: :disabled)
      assert_no_difference -> { McpOauthAuthorizationCode.count } do
        post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve")
      end
      assert_redirected_to login_path
      assert_nil session[:user_id]
    end

    test "approval revalidates the posted request" do
      client = create_client
      console_password_login(@operator.email)

      assert_no_difference -> { McpOauthAuthorizationCode.count } do
        post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve", redirect_uri: "https://evil.example/cb")
        assert_response :bad_request
        assert_nil response.location

        post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve", scope: "admin")
        assert_equal "invalid_scope", Rack::Utils.parse_nested_query(URI.parse(response.location).query).fetch("error")

        post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve", code_challenge: "")
        assert_equal "invalid_request", Rack::Utils.parse_nested_query(URI.parse(response.location).query).fetch("error")
      end
    end

    test "anything but an explicit approve is a denial" do
      client = create_client
      console_password_login(@operator.email)

      [ nil, "", "APPROVE", "yes" ].each do |decision|
        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: decision).compact
        end
        assert_equal "access_denied", Rack::Utils.parse_nested_query(URI.parse(response.location).query).fetch("error"), decision.inspect
      end
    end

    test "approval preserves the redirect_uri query and state" do
      registered = "https://client.example/cb?tenant=acme"
      client = create_client(redirect_uris: [ registered ])
      console_password_login(@operator.email)

      post "/mcp/oauth/authorize", params: authorize_params(client).merge(redirect_uri: registered, decision: "approve")
      redirect = URI.parse(response.location)
      assert_equal "client.example", redirect.host
      query = Rack::Utils.parse_nested_query(redirect.query)
      assert_equal "acme", query.fetch("tenant")
      assert_equal "state-test", query.fetch("state")
      assert query.fetch("code").present?
    end

    test "the consent form needs the session's authenticity token and a same-origin post" do
      client = create_client
      console_password_login(@operator.email)

      with_forgery_protection do
        get "/mcp/oauth/authorize", params: authorize_params(client)
        token = css_select("form input[name=authenticity_token]").first["value"]
        assert token.present?

        assert_no_difference -> { McpOauthAuthorizationCode.count } do
          post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve")
          assert_response :unprocessable_content

          post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve", authenticity_token: "forged")
          assert_response :unprocessable_content

          post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve", authenticity_token: token),
                                       headers: { "Origin" => "https://evil.example" }
          assert_response :unprocessable_content
        end

        assert_difference -> { McpOauthAuthorizationCode.count }, 1 do
          post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve", authenticity_token: token)
        end
        assert_response :redirect
        assert URI.parse(response.location).query.include?("code=")
      end
    end

    test "an existing client with a custom-scheme redirect still authorizes" do
      [
        "chrome-extension://abcdefghijklmnopabcdefghijklmnop/callback",
        "cursor://anysphere.cursor-mcp/oauth/callback",
        "com.example.legacy-app:/oauth2redirect"
      ].each do |stored|
        client = create_client
        client.update_column(:redirect_uris, [ stored ])
        console_password_login(@operator.email)
        params = authorize_params(client).merge(redirect_uri: stored)

        get "/mcp/oauth/authorize", params: params
        assert_response :ok, stored
        assert_select "form[action=?]", "/mcp/oauth/authorize"

        post "/mcp/oauth/authorize", params: params.merge(decision: "approve")
        assert_response :redirect, stored
        assert response.location.start_with?(stored.split(":").first + ":"), response.location
        code = Rack::Utils.parse_nested_query(URI.parse(response.location).query).fetch("code")

        post "/mcp/oauth/token", params: {
          grant_type: "authorization_code", client_id: client.public_client_id,
          code: code, redirect_uri: stored, code_verifier: code_verifier
        }
        assert_response :ok, stored
      end
    end

    test "upgrade compatibility: a consent form built exactly like origin/main's posts successfully" do
      client = create_client
      console_password_login(@operator.email)

      with_forgery_protection do
        get "/mcp/oauth/authorize", params: authorize_params(client)
        token = css_select("input[name=authenticity_token]").first["value"]

        # The exact body origin/main's authorize.html.erb submitted for Allow.
        body = {
          "authenticity_token" => token,
          "response_type" => "code",
          "client_id" => client.public_client_id,
          "redirect_uri" => redirect_uri,
          "scope" => "mcp:tools",
          "state" => "state-test",
          "resource" => "http://localhost:3000/mcp",
          "code_challenge" => code_challenge,
          "code_challenge_method" => "S256",
          "decision" => "approve"
        }
        assert_equal ORIGIN_MAIN_FORM_FIELDS.sort, (body.keys - [ "decision" ]).sort
        post "/mcp/oauth/authorize", params: body
      end

      assert_response :redirect
      redirect = URI.parse(response.location)
      assert_equal [ "127.0.0.1", 49152, "/callback" ], [ redirect.host, redirect.port, redirect.path ]
      query = Rack::Utils.parse_nested_query(redirect.query)
      assert_equal "state-test", query.fetch("state")
      exchange_authorization_code(client, query.fetch("code"))
      assert_response :ok
    end

    test "upgrade compatibility: a client registered and issued tokens before the upgrade refreshes" do
      stored_redirect = "chrome-extension://abcdefghijklmnopabcdefghijklmnop/callback"
      client = McpOauthClient.new(
        name: "Legacy MCP client",
        redirect_uris: [ stored_redirect ],
        grant_types: McpOauthClient::DEFAULT_GRANT_TYPES,
        response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
        scopes: McpOauthClient::DEFAULT_SCOPES,
        metadata: { "client_uri" => "https://client.example", "software_id" => "legacy" }
      )
      client.save!(validate: false)
      principal = ConsoleUserPrincipalProvisioner.call(@operator)
      legacy = McpOauthRefreshToken.create!(
        mcp_oauth_client: client, user: @operator, principal: principal,
        resource: "http://localhost:3000/mcp", scopes: [ "mcp:tools" ]
      )

      post "/mcp/oauth/token", params: {
        grant_type: "refresh_token", client_id: client.public_client_id, refresh_token: legacy.plaintext_token
      }

      assert_response :ok
      body = JSON.parse(response.body)
      assert_equal "Bearer", body.fetch("token_type")
      assert_equal "mcp:tools", body.fetch("scope")
      payload = decode_jwt_payload(body.fetch("access_token"))
      assert_equal client.public_client_id, payload.fetch("client_id")
      assert_equal principal.oid, payload.fetch("principal_id")
      assert legacy.reload.revoked_at.present?

      post "/mcp/oauth/token", params: {
        grant_type: "refresh_token", client_id: client.public_client_id, refresh_token: body.fetch("refresh_token")
      }
      assert_response :ok
    end

    test "authorization code exchange returns a JWT access token for the console principal" do
      client = create_client
      code = authorize_code(client)
      stored_code = McpOauthAuthorizationCode.find_usable(code)
      assert_equal @operator, stored_code.user
      assert_equal "http://localhost:3000/mcp", stored_code.resource
      assert_match(/\Aprn_/, stored_code.principal.oid)
      assert_equal "console_user", stored_code.principal.kind
      assert_equal @operator.id, stored_code.principal.console_user_id
      assert_empty stored_code.principal.labels.slice("console-user-id", "email")

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
      role = Role.find_by(foreign_id: "user-mcp")
      assert role, "expected the user-mcp role to be created"
      assert_equal "User MCP", role.name
      assert_equal "centaur", role.labels["managed-by"]
      assert_includes principal.roles, role
    end

    test "authorization approval identifies a principal from one Slack SSO identity" do
      @operator.user_identities.create!(
        provider: "slack", subject: "U0123456789", team_id: "T0123456789",
        email: @operator.email, email_verified: true
      )
      client = create_client

      code = authorize_code(client)

      principal = McpOauthAuthorizationCode.find_usable(code).principal
      assert_equal "U0123456789", principal.slack_user_id
      assert_equal "T0123456789", principal.slack_team_id
      assert_empty principal.labels.slice("slack_user_id", "slack_team_id")
    end

    test "authorization approval leaves Slack labels unset for ambiguous Slack SSO identities" do
      @operator.user_identities.create!(
        provider: "slack", subject: "U0123456789", team_id: "T0123456789",
        email: @operator.email, email_verified: true
      )
      @operator.user_identities.create!(
        provider: "slack", subject: "U9999999999", team_id: "T9999999999",
        email: @operator.email, email_verified: true
      )
      client = create_client

      code = authorize_code(client)

      principal = McpOauthAuthorizationCode.find_usable(code).principal
      assert_nil principal.slack_user_id
      assert_nil principal.slack_team_id
    end

    test "authorization approval leaves Slack fields unset for a malformed Slack user ID" do
      @operator.user_identities.create!(
        provider: "slack", subject: "U12345", team_id: "T0123456789",
        email: @operator.email, email_verified: true
      )
      client = create_client

      code = authorize_code(client)

      principal = McpOauthAuthorizationCode.find_usable(code).principal
      assert_nil principal.slack_user_id
      assert_nil principal.slack_team_id
    end

    test "authorization approval leaves Slack fields unset for a malformed Slack team ID" do
      @operator.user_identities.create!(
        provider: "slack", subject: "U0123456789", team_id: "TACME",
        email: @operator.email, email_verified: true
      )
      client = create_client

      code = authorize_code(client)

      principal = McpOauthAuthorizationCode.find_usable(code).principal
      assert_nil principal.slack_user_id
      assert_nil principal.slack_team_id
    end

    test "authorization approval reuses an existing user-mcp role" do
      existing = Role.create!(
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

    test "an authorization code cannot be replayed" do
      client = create_client
      code = authorize_code(client)
      exchange_authorization_code(client, code)
      assert_response :ok

      assert_no_difference -> { McpOauthRefreshToken.count } do
        exchange_authorization_code(client, code)
      end
      assert_response :bad_request
      assert_equal "invalid_grant", JSON.parse(response.body).fetch("error")
    end

    test "a refresh token cannot be replayed after rotation" do
      client = create_client
      exchange_authorization_code(client, authorize_code(client))
      first = JSON.parse(response.body).fetch("refresh_token")

      post "/mcp/oauth/token", params: { grant_type: "refresh_token", client_id: client.public_client_id, refresh_token: first }
      assert_response :ok

      post "/mcp/oauth/token", params: { grant_type: "refresh_token", client_id: client.public_client_id, refresh_token: first }
      assert_response :bad_request
      assert_equal "invalid_grant", JSON.parse(response.body).fetch("error")
    end

    test "the code exchange enforces PKCE, the client, and the redirect_uri" do
      client = create_client
      other = create_client
      code = authorize_code(client)

      post "/mcp/oauth/token", params: { grant_type: "authorization_code", client_id: client.public_client_id, code: code,
                                         redirect_uri: redirect_uri, code_verifier: "wrong-verifier" }
      assert_equal "PKCE verification failed.", JSON.parse(response.body).fetch("error_description")

      post "/mcp/oauth/token", params: { grant_type: "authorization_code", client_id: client.public_client_id, code: code,
                                         redirect_uri: redirect_uri }
      assert_equal "PKCE verification failed.", JSON.parse(response.body).fetch("error_description")

      post "/mcp/oauth/token", params: { grant_type: "authorization_code", client_id: other.public_client_id, code: code,
                                         redirect_uri: redirect_uri, code_verifier: code_verifier }
      assert_equal "Authorization code was not issued to this client.", JSON.parse(response.body).fetch("error_description")

      post "/mcp/oauth/token", params: { grant_type: "authorization_code", client_id: client.public_client_id, code: code,
                                         redirect_uri: "http://127.0.0.1:49152/other", code_verifier: code_verifier }
      assert_equal "redirect_uri does not match the authorization request.", JSON.parse(response.body).fetch("error_description")

      exchange_authorization_code(client, code)
      assert_response :ok
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
      console_password_login(@operator.email)

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

    # Rails disables CSRF checks in the test environment; the consent form
    # tests turn them back on, as in production.
    def with_forgery_protection
      previous = ActionController::Base.allow_forgery_protection
      ActionController::Base.allow_forgery_protection = true
      yield
    ensure
      ActionController::Base.allow_forgery_protection = previous
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
