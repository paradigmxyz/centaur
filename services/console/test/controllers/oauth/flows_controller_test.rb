require "test_helper"

module Oauth
  # Covers the consent flow end to end: /oauth/:slug/start builds the IdP redirect
  # and binds the browser; /oauth/:slug/callback exchanges the code, upserts a
  # BrokerCredential, and renders an iron-control result page. The IdP is faked by
  # swapping the controller's exchange_client_factory for a client wrapped around
  # an HTTP double returning a canned token response.
  class FlowsControllerTest < ActionDispatch::IntegrationTest
    CLIENT_ID = "acme-google-client-id".freeze

    setup do
      @app = oauth_apps(:acme_google) # slug "google"
      @app.update!(client_secret: "app-secret")
    end

    teardown do
      FlowsController.exchange_client_factory = -> { Broker::AuthorizationCodeClient.new }
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

    test "callback happy path mints a live credential and renders a success page" do
      state = start_flow
      stub_exchange(status: 200, body: token_body)

      assert_difference -> { BrokerCredential.count } => 1 do
        get oauth_callback_url(slug: "google"), params: { state: state, code: "auth-code" }
      end
      assert_response :ok
      assert_match "Connected", response.body
      assert_match "user@example.com", response.body

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
      assert_includes response.body, cred.oid
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

      assert_response :ok
      assert_match "Connected", response.body
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
