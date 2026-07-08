require "test_helper"

module Oauth
  # Covers the consent flow end to end: /oauth/:slug/start builds the IdP redirect
  # and binds the browser; /oauth/:slug/callback exchanges the code, upserts a
  # BrokerCredential, and renders an iron-control result page. The IdP is faked by
  # swapping the controller's exchange_client_factory for a client wrapped around
  # an HTTP double returning a canned token response.
  class FlowsControllerTest < ActionDispatch::IntegrationTest
    include ActiveJob::TestHelper

    CLIENT_ID = "acme-google-client-id".freeze
    SLACK_CLIENT_ID = "acme-slack-client-id".freeze
    GITHUB_CLIENT_ID = "acme-github-client-id".freeze
    ATTIO_CLIENT_ID = "acme-attio-client-id".freeze
    LINEAR_CLIENT_ID = "acme-linear-client-id".freeze

    setup do
      @app = oauth_apps(:acme_google) # slug "google"
      @app.update!(client_secret: "app-secret")
      oauth_apps(:acme_slack).update!(client_secret: "slack-secret")
      oauth_apps(:acme_github).update!(client_secret: "github-secret")
      oauth_apps(:acme_attio).update!(client_secret: "attio-secret")
      oauth_apps(:acme_linear).update!(client_secret: "linear-secret")
      clear_enqueued_jobs
    end

    teardown do
      FlowsController.exchange_client_factory = -> { Broker::AuthorizationCodeClient.new }
      clear_enqueued_jobs
    end

    class StubHTTP
      def initialize(status:, body:)
        @status = status
        @body = body
      end

      def call(url:, form:, headers:, timeout:)
        Broker::AuthorizationCodeClient::Response.new(status: @status, body: @body)
      end
    end

    def stub_exchange(status:, body:)
      FlowsController.exchange_client_factory = -> { Broker::AuthorizationCodeClient.new(http: StubHTTP.new(status: status, body: body)) }
    end

    def id_token(claims)
      "h.#{Base64.urlsafe_encode64(claims.to_json, padding: false)}.s"
    end

    def token_body(sub: "google-sub-1", email: "user@example.com", aud: CLIENT_ID,
                   iss: "https://accounts.google.com", scope: "https://www.googleapis.com/auth/gmail.readonly openid", **overrides)
      {
        access_token: "AT", refresh_token: "RT", expires_in: 3600, scope: scope,
        id_token: id_token({ "aud" => aud, "iss" => iss, "sub" => sub, "email" => email })
      }.merge(overrides).to_json
    end

    def slack_token_body(sub: "U0R7MFMJM", scope: "chat:write", id_token_value: nil, **overrides)
      {
        ok: true, access_token: "xoxe.xoxb-1-bot", refresh_token: "xoxe-1-bot-refresh",
        expires_in: 43_200, token_type: "bot", scope: "commands",
        id_token: id_token_value,
        authed_user: {
          id: sub,
          user: "grace",
          access_token: "xoxe.xoxp-1-user",
          refresh_token: "xoxe-1-refresh",
          expires_in: 43_200,
          scope: scope,
          token_type: "user"
        }
      }.merge(overrides).to_json
    end

    def github_token_body(scope: "repo,read:user", **overrides)
      {
        access_token: "gho-user-token",
        token_type: "bearer",
        scope: scope
      }.merge(overrides).to_json
    end

    def attio_token_body(**overrides)
      {
        access_token: "attio-user-token",
        token_type: "Bearer"
      }.merge(overrides).to_json
    end

    def linear_token_body(scope: "read write", **overrides)
      {
        access_token: "lin-user-token",
        refresh_token: "lin-refresh-token",
        token_type: "Bearer",
        expires_in: 86_399,
        scope: scope
      }.merge(overrides).to_json
    end

    def sign_in(user)
      post login_url, params: { email: user.email, password: "password123456" }
    end

    # Runs /start and returns the state extracted from the IdP redirect (the flow
    # cookie is set in the shared integration cookie jar as a side effect).
    def start_flow(slug: "google", **params)
      get oauth_start_url(slug: slug), params: params
      assert_response :redirect
      query = URI.parse(response.location).query
      URI.decode_www_form(query).to_h.fetch("state")
    end

    # --- start ----------------------------------------------------------------

    test "start redirects to Attio with dashboard-configured scopes" do
      get oauth_start_url(slug: "attio")
      assert_response :redirect
      uri = URI.parse(response.location)
      assert_equal "app.attio.com", uri.host
      assert_equal "/authorize", uri.path
      q = URI.decode_www_form(uri.query).to_h
      assert_equal ATTIO_CLIENT_ID, q["client_id"]
      assert_equal "http://www.example.com/oauth/attio/callback", q["redirect_uri"]
      assert_equal "code", q["response_type"]
      assert_equal "S256", q["code_challenge_method"]
      assert q["code_challenge"].present?
      # The Attio developer dashboard owns the effective scopes; the generic
      # flow still sends the sample app allowlist as a harmless scope param.
      assert_equal "record_permission:read object_configuration:read", q["scope"]
    end

    test "start redirects to Google with the right params and sets the flow cookie" do
      get oauth_start_url(slug: "google")
      assert_response :redirect
      uri = URI.parse(response.location)
      assert_equal "accounts.google.com", uri.host
      q = URI.decode_www_form(uri.query).to_h
      assert_equal CLIENT_ID, q["client_id"]
      assert_equal "http://www.example.com/oauth/google/callback", q["redirect_uri"]
      assert_equal "code", q["response_type"]
      assert_equal "offline", q["access_type"]
      assert_equal "consent", q["prompt"]
      assert_equal "S256", q["code_challenge_method"]
      assert q["code_challenge"].present?
      scopes = q["scope"].split
      assert_includes scopes, "https://www.googleapis.com/auth/gmail.readonly"
      assert_includes scopes, "openid"
      assert_includes scopes, "https://www.googleapis.com/auth/userinfo.email"
      state = Rails.application.message_verifier(FlowsController::STATE_PURPOSE)
                   .verified(q["state"], purpose: FlowsController::STATE_PURPOSE)
      assert_equal @app.oid, state["app"]
      assert response.cookies["oauth_flow"].present?
    end

    test "start redirects to Slack with comma separated scopes" do
      get oauth_start_url(slug: "slack")
      assert_response :redirect
      uri = URI.parse(response.location)
      assert_equal "slack.com", uri.host
      assert_equal "/oauth/v2/authorize", uri.path
      q = URI.decode_www_form(uri.query).to_h
      assert_equal SLACK_CLIENT_ID, q["client_id"]
      assert_equal "http://www.example.com/oauth/slack/callback", q["redirect_uri"]
      assert_equal "code", q["response_type"]
      assert_equal "S256", q["code_challenge_method"]
      assert_nil q["scope"]
      scopes = q["user_scope"].split(",")
      assert_includes scopes, "chat:write"
      assert_includes scopes, "channels:history"
      refute_includes scopes, "openid"
      refute_includes scopes, "email"
      refute_includes scopes, "profile"
    end

    test "start redirects to GitHub with space separated scopes" do
      get oauth_start_url(slug: "github")
      assert_response :redirect
      uri = URI.parse(response.location)
      assert_equal "github.com", uri.host
      assert_equal "/login/oauth/authorize", uri.path
      q = URI.decode_www_form(uri.query).to_h
      assert_equal GITHUB_CLIENT_ID, q["client_id"]
      assert_equal "http://www.example.com/oauth/github/callback", q["redirect_uri"]
      assert_equal "code", q["response_type"]
      assert_equal "S256", q["code_challenge_method"]
      assert_nil q["user_scope"]
      scopes = q["scope"].split
      assert_includes scopes, "repo"
      assert_includes scopes, "read:user"
    end

    test "start redirects to Linear with comma separated scopes" do
      get oauth_start_url(slug: "linear")
      assert_response :redirect
      uri = URI.parse(response.location)
      assert_equal "linear.app", uri.host
      assert_equal "/oauth/authorize", uri.path
      q = URI.decode_www_form(uri.query).to_h
      assert_equal LINEAR_CLIENT_ID, q["client_id"]
      assert_equal "http://www.example.com/oauth/linear/callback", q["redirect_uri"]
      assert_equal "code", q["response_type"]
      assert_equal "S256", q["code_challenge_method"]
      assert_nil q["user_scope"]
      assert_nil q["prompt"]
      scopes = q["scope"].split(",")
      assert_includes scopes, "read"
      assert_includes scopes, "write"
    end

    test "start works without any session" do
      get oauth_start_url(slug: "google")
      assert_response :redirect
      assert_nil session[:user_id]
    end

    test "start works with a pending console session" do
      sign_in users(:pending_user)

      get oauth_start_url(slug: "google")

      assert_response :redirect
      assert_equal "accounts.google.com", URI.parse(response.location).host
    end

    test "start 404s an unknown slug" do
      get oauth_start_url(slug: "nope")
      assert_response :not_found
      assert_match "Unknown integration", response.body
    end

    test "start renders a 422 result page for a disabled app" do
      get oauth_start_url(slug: "google-disabled")
      assert_response :unprocessable_entity
      assert_match "disabled", response.body
    end

    test "start 422s a scope outside the allowlist" do
      get oauth_start_url(slug: "google"), params: { scopes: "https://www.googleapis.com/auth/drive" }
      assert_response :unprocessable_entity
    end

    test "start honors an optional scopes subset" do
      get oauth_start_url(slug: "google"), params: { scopes: "https://www.googleapis.com/auth/calendar.readonly" }
      assert_response :redirect
      scopes = URI.decode_www_form(URI.parse(response.location).query).to_h["scope"].split
      assert_includes scopes, "https://www.googleapis.com/auth/calendar.readonly"
      refute_includes scopes, "https://www.googleapis.com/auth/gmail.readonly"
    end

    # --- callback -------------------------------------------------------------

    test "callback happy path mints a live credential and redirects to the Integrations page" do
      state = start_flow
      stub_exchange(status: 200, body: token_body)

      assert_difference -> { BrokerCredential.count } => 1 do
        get oauth_callback_url(slug: "google"), params: { state: state, code: "auth-code" }
      end
      assert_redirected_to console_integrations_path
      assert_equal "google connected as user@example.com.", flash[:notice]

      cred = BrokerCredential.find_by(oauth_app: @app, provider_subject: "google-sub-1")
      assert_equal "acme", cred.namespace
      assert_equal "google-google-google-sub-1", cred.foreign_id
      assert_equal "https://oauth2.googleapis.com/token", cred.token_endpoint
      assert_equal "user@example.com", cred.provider_email
      assert cred.external_user_key.present?, "external_user_key should be auto-generated"
      assert_equal %w[https://www.googleapis.com/auth/gmail.readonly openid], cred.scopes
      assert_equal "live", cred.status
      assert_equal "AT", cred.access_token
      assert_equal "RT", cred.refresh_token
      assert cred.next_attempt_at.present?
      assert_nil cred.created_by
    end

    test "callback happy path supports Slack user tokens" do
      state = start_flow(slug: "slack", scopes: "chat:write")
      stub_exchange(status: 200, body: slack_token_body)

      assert_difference -> { BrokerCredential.count } => 1 do
        get oauth_callback_url(slug: "slack"), params: { state: state, code: "auth-code" }
      end
      assert_redirected_to console_integrations_path
      assert_match(/\Aslack connected/, flash[:notice])

      app = oauth_apps(:acme_slack)
      cred = BrokerCredential.find_by(oauth_app: app, provider_subject: "U0R7MFMJM")
      assert_equal "acme", cred.namespace
      assert_equal "slack-slack-u0r7mfmjm", cred.foreign_id
      assert_equal "Slack – grace", cred.name
      assert_equal "https://slack.com/api/oauth.v2.access", cred.token_endpoint
      assert_nil cred.provider_email
      assert_equal %w[chat:write], cred.scopes
      assert_equal "xoxe.xoxp-1-user", cred.access_token
      assert_equal "xoxe-1-refresh", cred.refresh_token
      assert_equal [ "slack.com" ], cred.static_secret.rules.map(&:host)
      assert_equal "Slack – grace token", cred.static_secret.name
    end

    test "callback happy path supports Attio workspace tokens" do
      state = start_flow(slug: "attio", scopes: "record_permission:read")
      stub_exchange(status: 200, body: attio_token_body)

      assert_enqueued_with(job: Oauth::EnrichAttioCredentialIdentityJob) do
        assert_difference -> { BrokerCredential.count } => 1 do
          get oauth_callback_url(slug: "attio"), params: { state: state, code: "auth-code" }
        end
      end
      assert_redirected_to console_integrations_path
      assert_match(/\Aattio connected/, flash[:notice])

      app = oauth_apps(:acme_attio)
      cred = BrokerCredential.find_by(oauth_app: app)
      assert_equal "acme", cred.namespace
      assert_match(/\Aattio-attio-pending-[a-f0-9]{32}\z/, cred.foreign_id)
      assert_match(/\Apending-[a-f0-9]{32}\z/, cred.provider_subject)
      assert_equal "Attio – Pending Attio workspace", cred.name
      assert_equal "https://app.attio.com/oauth/token", cred.token_endpoint
      assert_nil cred.provider_email
      assert_equal %w[record_permission:read], cred.scopes
      assert_equal "attio-user-token", cred.access_token
      assert_nil cred.refresh_token
      assert_nil cred.next_attempt_at
      assert_equal [ "api.attio.com" ], cred.static_secret.rules.map(&:host)
      assert_equal "Attio – Pending Attio workspace token", cred.static_secret.name
      refute_includes BrokerCredential.refreshable, cred
    end

    test "callback happy path supports GitHub OAuth app tokens" do
      state = start_flow(slug: "github", scopes: "repo read:user")
      stub_exchange(status: 200, body: github_token_body)

      assert_enqueued_with(job: Oauth::EnrichGithubCredentialIdentityJob) do
        assert_difference -> { BrokerCredential.count } => 1 do
          get oauth_callback_url(slug: "github"), params: { state: state, code: "auth-code" }
        end
      end
      assert_redirected_to console_integrations_path
      assert_match(/\Agithub connected/, flash[:notice])

      app = oauth_apps(:acme_github)
      cred = BrokerCredential.find_by(oauth_app: app)
      assert_equal "acme", cred.namespace
      assert_match(/\Agithub-github-pending-[a-f0-9]{32}\z/, cred.foreign_id)
      assert_match(/\Apending-[a-f0-9]{32}\z/, cred.provider_subject)
      assert_equal "GitHub – Pending GitHub account", cred.name
      assert_equal "https://github.com/login/oauth/access_token", cred.token_endpoint
      assert_nil cred.provider_email
      assert_equal %w[repo read:user], cred.scopes
      assert_equal "gho-user-token", cred.access_token
      assert_nil cred.refresh_token
      assert_nil cred.next_attempt_at
      assert_equal [ "api.github.com", "github.com" ], cred.static_secret.rules.map(&:host)
      assert_equal "GitHub – Pending GitHub account token", cred.static_secret.name
      refute_includes BrokerCredential.refreshable, cred
    end

    test "callback happy path supports Linear OAuth app tokens" do
      state = start_flow(slug: "linear", scopes: "read write")
      stub_exchange(status: 200, body: linear_token_body)

      assert_enqueued_with(job: Oauth::EnrichLinearCredentialIdentityJob) do
        assert_difference -> { BrokerCredential.count } => 1 do
          get oauth_callback_url(slug: "linear"), params: { state: state, code: "auth-code" }
        end
      end
      assert_redirected_to console_integrations_path
      assert_match(/\Alinear connected/, flash[:notice])

      app = oauth_apps(:acme_linear)
      cred = BrokerCredential.find_by(oauth_app: app)
      assert_equal "acme", cred.namespace
      assert_match(/\Alinear-linear-pending-[a-f0-9]{32}\z/, cred.foreign_id)
      assert_match(/\Apending-[a-f0-9]{32}\z/, cred.provider_subject)
      assert_equal "Linear – Pending Linear account", cred.name
      assert_equal "https://api.linear.app/oauth/token", cred.token_endpoint
      assert_nil cred.provider_email
      assert_equal %w[read write], cred.scopes
      assert_equal "lin-user-token", cred.access_token
      assert_equal "lin-refresh-token", cred.refresh_token
      assert cred.next_attempt_at.present?
      assert_equal [ "api.linear.app" ], cred.static_secret.rules.map(&:host)
      assert_equal "Linear – Pending Linear account token", cred.static_secret.name
    end

    test "callback wraps the minted credential in a grantable static secret" do
      state = start_flow
      stub_exchange(status: 200, body: token_body)

      assert_difference -> { StaticSecret.count } => 1 do
        get oauth_callback_url(slug: "google"), params: { state: state, code: "auth-code" }
      end

      cred = BrokerCredential.find_by(oauth_app: @app, provider_subject: "google-sub-1")
      secret = cred.static_secret
      assert_equal cred, secret.broker_credential # first-class link to the credential
      assert_equal cred.namespace, secret.namespace
      assert_nil secret.foreign_id # found by association, so no collidable foreign_id
      assert_nil secret.created_by # the unauthenticated flow has no operator
      assert_equal({ "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" }, secret.inject_config)
      assert_equal "token_broker", secret.source.source_type
      assert_equal cred.oid, secret.source.config["credential_id"]
      assert_equal [ "*.googleapis.com" ], secret.rules.map(&:host)
      # The source resolves the credential's live token at sync time.
      assert_equal({ "type" => "control_plane", "value" => "AT" }, secret.source.to_proxy_source)
    end

    test "re-consent neither duplicates the wrapping secret nor clobbers operator edits" do
      state1 = start_flow
      stub_exchange(status: 200, body: token_body)
      get oauth_callback_url(slug: "google"), params: { state: state1, code: "code-1" }
      cred = BrokerCredential.find_by(oauth_app: @app, provider_subject: "google-sub-1")
      secret = cred.static_secret
      assert_not_nil secret
      secret.update!(name: "operator-renamed")

      state2 = start_flow
      stub_exchange(status: 200, body: token_body)
      assert_no_difference -> { StaticSecret.count } do
        get oauth_callback_url(slug: "google"), params: { state: state2, code: "code-2" }
      end
      assert_equal "operator-renamed", secret.reload.name
    end

    test "callback records the signed-in user on the credential and keeps the original owner on re-consent" do
      user = users(:member_user)
      sign_in user
      state = start_flow
      stub_exchange(status: 200, body: token_body)
      get oauth_callback_url(slug: "google"), params: { state: state, code: "auth-code" }

      cred = BrokerCredential.find_by(oauth_app: @app, provider_subject: "google-sub-1")
      assert_equal user, cred.created_by

      # Someone else re-consenting for the same provider account does not steal
      # the credential.
      sign_in users(:acme_admin)
      state = start_flow
      stub_exchange(status: 200, body: token_body)
      get oauth_callback_url(slug: "google"), params: { state: state, code: "auth-code" }
      assert_equal user, cred.reload.created_by
    end

    test "a Slack consent with no email in the token response still shows connected on Integrations" do
      user = users(:member_user)
      sign_in user
      state = start_flow(slug: "slack", scopes: "chat:write")
      stub_exchange(status: 200, body: slack_token_body)
      get oauth_callback_url(slug: "slack"), params: { state: state, code: "auth-code" }
      assert_redirected_to console_integrations_path

      # Slack's token response carries no email (enrichment fills it in later),
      # so the connected state must come from the created_by link.
      cred = BrokerCredential.find_by(oauth_app: oauth_apps(:acme_slack), provider_subject: "U0R7MFMJM")
      assert_nil cred.provider_email
      assert_equal user, cred.created_by

      get console_integrations_url
      assert_select "a.btn-secondary[href=?]", "http://www.example.com/oauth/slack/start", text: "Reconnect"
    end

    test "callback works with a disabled console session" do
      user = users(:member_user)
      sign_in user
      state = start_flow
      user.update!(status: :disabled)
      stub_exchange(status: 200, body: token_body)

      assert_difference -> { BrokerCredential.count }, 1 do
        get oauth_callback_url(slug: "google"), params: { state: state, code: "auth-code" }
      end

      assert_redirected_to console_integrations_path
      assert_match(/connected/, flash[:notice])
      assert_equal user.id, session[:user_id]
    end

    test "re-consent for the same account updates the existing credential and revives a dead one" do
      state1 = start_flow
      stub_exchange(status: 200, body: token_body(email: "old@example.com"))
      get oauth_callback_url(slug: "google"), params: { state: state1, code: "code-1" }
      cred = BrokerCredential.find_by(oauth_app: @app, provider_subject: "google-sub-1")
      original_user_key = cred.external_user_key
      cred.update!(dead: true, dead_reason: "invalid_grant")

      state2 = start_flow
      stub_exchange(status: 200, body: token_body(email: "new@example.com"))
      assert_no_difference -> { BrokerCredential.count } do
        get oauth_callback_url(slug: "google"), params: { state: state2, code: "code-2" }
      end
      cred.reload
      assert_equal "new@example.com", cred.provider_email
      assert_equal original_user_key, cred.external_user_key # preserved on re-consent
      refute cred.dead?
      assert_equal "live", cred.status
    end

    test "callback with error=access_denied renders a denied page and mints nothing" do
      state = start_flow
      assert_no_difference -> { BrokerCredential.count } do
        get oauth_callback_url(slug: "google"), params: { state: state, error: "access_denied" }
      end
      assert_response :unprocessable_entity
      assert_match "Not connected", response.body
      assert_match "access_denied", response.body
    end

    test "callback with tampered state renders an error page and mints nothing" do
      assert_no_difference -> { BrokerCredential.count } do
        get oauth_callback_url(slug: "google"), params: { state: "tampered", code: "x" }
      end
      assert_response :bad_request
      assert_match "Something went wrong", response.body
    end

    test "callback with a missing flow cookie renders an error page" do
      state = Rails.application.message_verifier(FlowsController::STATE_PURPOSE).generate(
        { "app" => @app.oid, "scopes" => Array(@app.allowed_scopes), "nonce" => "some-nonce" },
        purpose: FlowsController::STATE_PURPOSE, expires_in: 10.minutes
      )
      assert_no_difference -> { BrokerCredential.count } do
        get oauth_callback_url(slug: "google"), params: { state: state, code: "x" }
      end
      assert_response :bad_request
    end

    test "callback exchange failure renders an error page and mints nothing" do
      state = start_flow
      stub_exchange(status: 400, body: { error: "invalid_grant" }.to_json)
      assert_no_difference -> { BrokerCredential.count } do
        get oauth_callback_url(slug: "google"), params: { state: state, code: "bad-code" }
      end
      assert_response :unprocessable_entity
      assert_match "invalid_grant", response.body
    end

    test "callback with id_token aud mismatch is treated as an error" do
      state = start_flow
      stub_exchange(status: 200, body: token_body(aud: "someone-else"))
      assert_no_difference -> { BrokerCredential.count } do
        get oauth_callback_url(slug: "google"), params: { state: state, code: "code" }
      end
      assert_response :unprocessable_entity
      assert_match "id_token_aud_mismatch", response.body
    end

    test "callback 404s when the app slug no longer exists" do
      state = start_flow
      @app.destroy!
      get oauth_callback_url(slug: "google"), params: { state: state, code: "code" }
      assert_response :not_found
    end
  end
end
