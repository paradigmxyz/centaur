require "test_helper"

# Covers console SSO login end to end: /auth/:provider/start builds the IdP
# redirect and binds the browser; /auth/:provider/callback exchanges the code,
# provisions/links a User, and establishes the session. The IdP is faked by
# swapping the controller's exchange_client_factory for a client wrapped around an
# HTTP double returning a canned token response (mirrors the broker flow test).
class SessionOauthControllerTest < ActionDispatch::IntegrationTest
  GOOGLE_CLIENT_ID = "google-login-client-id".freeze
  ENV_KEYS = %w[
    CENTAUR_CONSOLE_GOOGLE_CLIENT_ID CENTAUR_CONSOLE_GOOGLE_CLIENT_SECRET
    CENTAUR_CONSOLE_BOOTSTRAP_ADMINS CENTAUR_CONSOLE_SSO_EMAIL_DOMAINS
  ].freeze

  setup do
    @prev_env = ENV.to_hash.slice(*ENV_KEYS)
    ENV["CENTAUR_CONSOLE_GOOGLE_CLIENT_ID"] = GOOGLE_CLIENT_ID
    ENV["CENTAUR_CONSOLE_GOOGLE_CLIENT_SECRET"] = "google-login-secret"
    ENV["CENTAUR_CONSOLE_BOOTSTRAP_ADMINS"] = "boss@acme.example"
  end

  teardown do
    ENV_KEYS.each { |k| ENV.delete(k) }
    @prev_env.each { |k, v| ENV[k] = v }
    SessionOauthController.exchange_client_factory = -> { Broker::AuthorizationCodeClient.new }
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
    SessionOauthController.exchange_client_factory = -> { Broker::AuthorizationCodeClient.new(http: StubHTTP.new(status: status, body: body)) }
  end

  def id_token(claims)
    "h.#{Base64.urlsafe_encode64(claims.to_json, padding: false)}.s"
  end

  def token_body(sub:, email:, email_verified: true, name: "Test User",
                 aud: GOOGLE_CLIENT_ID, iss: "https://accounts.google.com")
    {
      access_token: "AT",
      id_token: id_token({ "aud" => aud, "iss" => iss, "sub" => sub,
                           "email" => email, "email_verified" => email_verified, "name" => name })
    }.to_json
  end

  # Runs /start and returns the signed state from the IdP redirect (the flow
  # cookie is set in the shared integration cookie jar as a side effect).
  def start_flow(provider: "google")
    get auth_start_url(provider: provider)
    assert_response :redirect
    URI.decode_www_form(URI.parse(response.location).query).to_h.fetch("state")
  end

  def run_callback(sub:, email:, provider: "google", **token_overrides)
    stub_exchange(status: 200, body: token_body(sub: sub, email: email, **token_overrides))
    state = start_flow(provider: provider)
    get auth_callback_url(provider: provider), params: { code: "the-code", state: state }
  end

  # --- login page -----------------------------------------------------------

  test "the login form offers a button for each configured provider" do
    get login_url
    assert_response :ok
    assert_select "a[href=?]", auth_start_path(provider: "google"), text: /Continue with Google/
    # Slack has no credentials configured, so it must not appear.
    assert_select "a[href=?]", auth_start_path(provider: "slack"), count: 0
  end

  # --- start ----------------------------------------------------------------

  test "start redirects to Google with login params and no offline access" do
    get auth_start_url(provider: "google")
    assert_response :redirect
    uri = URI.parse(response.location)
    assert_equal "accounts.google.com", uri.host
    q = URI.decode_www_form(uri.query).to_h
    assert_equal GOOGLE_CLIENT_ID, q["client_id"]
    assert_equal "http://www.example.com/auth/google/callback", q["redirect_uri"]
    assert_equal "code", q["response_type"]
    assert_equal "openid email profile", q["scope"]
    assert_equal "S256", q["code_challenge_method"]
    assert_nil q["access_type"], "login must not request offline access"
    assert_nil q["prompt"]
  end

  test "start rejects an unconfigured provider" do
    get auth_start_url(provider: "slack") # no slack creds set
    assert_redirected_to login_path
    assert_equal "That sign-in method is not available.", flash[:alert]
  end

  # --- callback: provisioning ------------------------------------------------

  test "callback provisions an active user for a non-bootstrap email and lands on the console" do
    assert_difference -> { User.count }, 1 do
      run_callback(sub: "new-sub", email: "newcomer@example.com")
    end
    assert_redirected_to console_threads_path
    user = User.find_by(email: "newcomer@example.com")
    assert user.active?
    assert_not user.admin?
    assert_equal "Test User", user.name
    assert_equal user.id, session[:user_id]
    assert_equal [ [ "google", "new-sub" ] ], user.user_identities.pluck(:provider, :subject)
  end

  test "callback provisions a user inside the configured SSO domain allowlist" do
    ENV["CENTAUR_CONSOLE_SSO_EMAIL_DOMAINS"] = "example.com acme.example"
    assert_difference -> { User.count }, 1 do
      run_callback(sub: "allowed-sub", email: "newcomer@example.com")
    end
    assert_redirected_to console_threads_path
    assert_equal User.find_by!(email: "newcomer@example.com").id, session[:user_id]
  end

  test "callback rejects a user outside the configured SSO domain allowlist" do
    ENV["CENTAUR_CONSOLE_SSO_EMAIL_DOMAINS"] = "acme.example"
    assert_no_difference -> { User.count } do
      assert_no_difference -> { UserIdentity.count } do
        run_callback(sub: "outside-sub", email: "newcomer@example.com")
      end
    end
    assert_redirected_to login_path
    assert_equal "That email domain is not allowed to access the console.", flash[:alert]
    assert_nil session[:user_id]
  end

  test "callback makes a bootstrap-allowlisted email active and admin" do
    run_callback(sub: "boss-sub", email: "boss@acme.example")
    assert_redirected_to console_principals_path
    user = User.find_by(email: "boss@acme.example")
    assert user.active?
    assert user.admin?
  end

  test "callback links a new identity to an existing user by verified email" do
    existing = users(:globex_admin)
    assert_no_difference -> { User.count } do
      assert_difference -> { existing.user_identities.count }, 1 do
        run_callback(sub: "fresh-sub", email: existing.email, email_verified: true)
      end
    end
    assert_redirected_to console_principals_path
    assert_equal existing.id, session[:user_id]
  end

  test "callback does not let an unverified email adopt an existing account" do
    existing = users(:globex_admin)
    assert_no_difference -> { User.count } do
      assert_no_difference -> { existing.user_identities.count } do
        run_callback(sub: "spoof-sub", email: existing.email, email_verified: false)
      end
    end
    assert_redirected_to login_path
    assert_nil session[:user_id]
  end

  test "callback creates an active user for an unverified, unrecognized email" do
    assert_difference -> { User.count }, 1 do
      run_callback(sub: "unv-sub", email: "stranger@example.com", email_verified: false)
    end
    user = User.find_by(email: "stranger@example.com")
    assert user.active?
    assert_not user.user_identities.first.email_verified
  end

  test "callback signs a returning identity into the same user" do
    identity = user_identities(:acme_admin_google)
    assert_no_difference -> { User.count } do
      assert_no_difference -> { UserIdentity.count } do
        run_callback(sub: identity.subject, email: identity.email)
      end
    end
    assert_redirected_to console_principals_path
    assert_equal identity.user_id, session[:user_id]
  end

  # --- callback: rejections --------------------------------------------------

  test "callback rejects a tampered or missing state" do
    get auth_callback_url(provider: "google"), params: { code: "x", state: "not-a-real-state" }
    assert_redirected_to login_path
    assert_nil session[:user_id]
  end

  test "callback treats an IdP error param as a cancellation" do
    state = start_flow
    get auth_callback_url(provider: "google"), params: { state: state, error: "access_denied" }
    assert_redirected_to login_path
    assert_equal "Sign in was canceled.", flash[:alert]
    assert_nil session[:user_id]
  end

  test "callback surfaces an exchange failure without signing in" do
    stub_exchange(status: 400, body: { error: "invalid_grant" }.to_json)
    state = start_flow
    get auth_callback_url(provider: "google"), params: { code: "bad", state: state }
    assert_redirected_to login_path
    assert_nil session[:user_id]
  end
end
