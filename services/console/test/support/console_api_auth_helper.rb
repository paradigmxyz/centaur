# Helpers for /api/v1 request tests that exercise the authentication paths
# an endpoint declares: the console session cookie (with CSRF) and/or bearer
# API keys.
#
#   sign_in_console(users(:acme_admin))         # cookie session in this test
#   get api_v1_principals_path                   # GET needs no CSRF token
#   post api_v1_principals_path, params: {...}, headers: csrf_headers, as: :json
#   get api_v1_principals_path, headers: bearer_headers("iak_acme-ci-token")
#
#   assert_admin_only_api :post, api_v1_principals_path, params: { data: {} }
#   assert_active_user_api :get, api_v1_skills_path
module ConsoleApiAuthHelper
  TEST_PASSWORD = "password123456".freeze
  ADMIN_TOKEN = "iak_acme-ci-token".freeze
  MEMBER_TOKEN = "iak_member-token".freeze
  DISABLED_TOKEN = "iak_disabled-token".freeze

  # Signs +user+ in the way the login page does (the POST /login form),
  # establishing the console session cookie for the current integration
  # session.
  def sign_in_console(user, password: TEST_PASSWORD)
    console_password_login(user.email, password)
    user
  end

  # POST /login for +email+, as the server-rendered login form submits it
  # (the test environment skips the authenticity token; the CSRF tests in
  # sessions_controller_test turn forgery protection on). The response (a
  # redirect, or the re-rendered form) is left for the caller to inspect.
  def console_password_login(email, password = TEST_PASSWORD)
    post login_path, params: { email: email, password: password }
    @console_csrf_token = nil
  end

  # DELETE /api/v1/session, as the SPA's sign-out button does.
  def console_sign_out
    delete api_v1_session_path, headers: csrf_headers
    @console_csrf_token = nil
  end

  # Where the last console_password_login sent the browser (path and query).
  def console_login_redirect
    location = response.location || raise("no redirect: #{response.status}")
    URI.parse(location).request_uri
  end

  # The session's CSRF token, as the SPA obtains it from GET /api/v1/me.
  def console_csrf_token
    @console_csrf_token ||= begin
      get api_v1_me_path
      JSON.parse(response.body).dig("data", "csrf_token") ||
        raise("no CSRF token: is the session signed in? (#{response.status})")
    end
  end

  def csrf_headers(token = console_csrf_token)
    { "X-CSRF-Token" => token }
  end

  def bearer_headers(token = ADMIN_TOKEN)
    { "Authorization" => "Bearer #{token}" }
  end

  def json_body
    JSON.parse(response.body)
  end

  # A fresh active user, so a test can sign in and then flip their status or
  # admin flag without disturbing fixtures used elsewhere in the test.
  def create_console_user(admin: false, status: :active, email: nil)
    User.create!(
      email: email || "user-#{SecureRandom.hex(4)}@example.test",
      password: TEST_PASSWORD,
      status: status,
      admin: admin
    )
  end

  # Runs +method+ +path+ in a separate integration session per caller and
  # asserts the admin-only contract for the endpoint's declared credential
  # policy (Api::BaseController.accepts_credentials; pass +credential:+ to
  # override it for a request-dependent action):
  #
  # - always: anonymous 401;
  # - bearer accepted: bad and disabled bearer 401, non-admin bearer 403;
  #   otherwise any bearer (even an admin's) 403 session_required;
  # - session accepted: disabled cookie 401, pending cookie 403, non-admin
  #   cookie 403, descoped admin 403, and (for writes) admin cookie without
  #   CSRF 403; otherwise an admin cookie 403 bearer_required.
  #
  # Success paths are left to the caller because they have side effects.
  def assert_admin_only_api(method, path, params: nil, credential: nil)
    policy = credential || declared_credential_policy(method, path)
    assert_api_denials(method, path, params: params, policy: policy)

    if accepts_bearer?(policy)
      assert_api_status(:forbidden, method, path, params: params, reason: "admin_required",
                        headers: bearer_headers(MEMBER_TOKEN))
    end
    return unless accepts_session?(policy)

    assert_api_status(:forbidden, method, path, params: params, reason: "admin_required",
                      sign_in: create_console_user, csrf: true)
    assert_api_status(:forbidden, method, path, params: params, reason: "admin_required",
                      sign_in: create_console_user(admin: true), csrf: true, descope: true)
    unless %w[get head].include?(method.to_s)
      assert_api_status(:forbidden, method, path, params: params, reason: "csrf",
                        sign_in: create_console_user(admin: true), csrf: false)
    end
  end

  # Like assert_admin_only_api for endpoints that opt in with
  # allow_active_users: anonymous, disabled, pending, wrong-credential, and
  # CSRF-less writes are refused, and so are non-admin bearer keys.
  def assert_active_user_api(method, path, params: nil, credential: nil)
    policy = credential || declared_credential_policy(method, path)
    assert_api_denials(method, path, params: params, policy: policy)
    # allow_active_users widens session access only: keys need an admin owner.
    if accepts_bearer?(policy)
      assert_api_status(:forbidden, method, path, params: params, reason: "admin_required",
                        headers: bearer_headers(MEMBER_TOKEN))
    end
    if accepts_session?(policy) && !%w[get head].include?(method.to_s)
      assert_api_status(:forbidden, method, path, params: params, reason: "csrf",
                        sign_in: create_console_user, csrf: false)
    end
  end

  # The credential policy the routed controller declares for +method+ +path+.
  def declared_credential_policy(method, path)
    route = Rails.application.routes.recognize_path(path.to_s.split("?").first, method: method.to_s.upcase)
    controller = "#{route[:controller]}_controller".camelize.constantize
    controller.accepted_credentials_for(route[:action]) ||
      flunk("#{controller}##{route[:action]} declares no accepts_credentials")
  end

  private

  def accepts_bearer?(policy) = %i[bearer bearer_or_session].include?(policy)
  def accepts_session?(policy) = %i[session bearer_or_session].include?(policy)

  def assert_api_denials(method, path, params:, policy:)
    assert_api_status(:unauthorized, method, path, params: params)
    if accepts_bearer?(policy)
      assert_api_status(:unauthorized, method, path, params: params, headers: bearer_headers("iak_bogus"))
      assert_api_status(:unauthorized, method, path, params: params, headers: bearer_headers(DISABLED_TOKEN))
    else
      assert_api_status(:forbidden, method, path, params: params, reason: "session_required",
                        headers: bearer_headers(ADMIN_TOKEN))
      assert_api_status(:forbidden, method, path, params: params, reason: "session_required",
                        headers: bearer_headers("iak_bogus"))
    end
    if accepts_session?(policy)
      assert_api_status(:unauthorized, method, path, params: params, reason: "account_disabled",
                        sign_in: create_console_user, csrf: true, disable: true)
      assert_api_status(:forbidden, method, path, params: params, reason: "account_pending",
                        sign_in: create_console_user(status: :pending), csrf: false)
    else
      assert_api_status(:forbidden, method, path, params: params, reason: "bearer_required",
                        sign_in: create_console_user(admin: true), csrf: true)
    end
  end

  def assert_api_status(status, method, path, params: nil, headers: {}, sign_in: nil, csrf: false,
                        descope: false, disable: false, reason: nil)
    session = open_session
    session.extend(ConsoleApiAuthHelper)
    if sign_in
      session.sign_in_console(sign_in)
      headers = headers.merge(session.csrf_headers) if csrf
      session.post(api_v1_descope_path, headers: session.csrf_headers) if descope
      sign_in.update!(status: :disabled) if disable
    end
    session.public_send(method, path, params: params, headers: headers, as: (:json if params))
    label = "#{method.upcase} #{path} (#{sign_in ? "cookie #{sign_in.status}#{" admin" if sign_in.admin?}" : headers.keys.join(",").presence || "anonymous"})"
    assert_equal Rack::Utils.status_code(status), session.response.status, "#{label}: #{session.response.body}"
    if reason
      assert_equal reason, JSON.parse(session.response.body).dig("error", "details", "reason"), label
    end
  end
end
