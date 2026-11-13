# Helpers for /api/v1 request tests. The namespace decides the credential:
# /api/v1/console/* takes the console session cookie (with CSRF on writes),
# the rest of /api/v1 (outside /api/v1/sandbox) a bearer API key.
#
#   sign_in_console(users(:acme_admin))                # cookie session in this test
#   get api_v1_console_principals_path                 # GET needs no CSRF token
#   post api_v1_console_principals_path, params: {...}, headers: csrf_headers, as: :json
#   get api_v1_principals_path, headers: bearer_headers("iak_acme-ci-token")
#
#   assert_admin_only_api :post, api_v1_console_principals_path, params: { data: {} }
#   assert_active_user_api :get, api_v1_console_skills_path
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

  # POST /login for +email+. The response (a redirect, or the re-rendered
  # form) is left for the caller to inspect.
  def console_password_login(email, password = TEST_PASSWORD)
    post login_path, params: { email: email, password: password }
    @console_csrf_token = nil
  end

  def console_sign_out
    delete api_v1_console_session_path, headers: csrf_headers
    @console_csrf_token = nil
  end

  def console_login_redirect
    location = response.location || raise("no redirect: #{response.status}")
    URI.parse(location).request_uri
  end

  # The session's CSRF token, as the SPA obtains it from GET /api/v1/console/me.
  def console_csrf_token
    @console_csrf_token ||= begin
      get api_v1_console_me_path
      JSON.parse(response.body).dig("data", "csrf_token") ||
        raise("no CSRF token: is the session signed in? (#{response.status})")
    end
  end

  # What the SPA sends on writes: the session's CSRF token and this origin.
  def csrf_headers(token = console_csrf_token)
    { "X-CSRF-Token" => token, "Origin" => "http://www.example.com" }
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
  # asserts the admin-only contract for the credential the path's namespace
  # takes (see #path_credential):
  #
  # - console (session): anonymous 401 session_required; any bearer, even an
  #   admin's, 403 session_required; disabled cookie 401, pending cookie 403,
  #   non-admin cookie 403, and (for writes) admin cookie without CSRF 403;
  # - bearer: anonymous, bad, and disabled bearer 401; non-admin bearer 403;
  #   an admin's console cookie 401 (it is not a credential here).
  #
  # Success paths are left to the caller because they have side effects.
  def assert_admin_only_api(method, path, params: nil)
    credential = path_credential(path)
    assert_api_denials(method, path, params: params, credential: credential)

    if credential == :bearer
      assert_api_status(:forbidden, method, path, params: params, message: "API key owner is not an admin",
                        headers: bearer_headers(MEMBER_TOKEN))
      return
    end

    assert_api_status(:forbidden, method, path, params: params, reason: "admin_required",
                      sign_in: create_console_user, csrf: true)
    unless %w[get head].include?(method.to_s)
      assert_api_status(:forbidden, method, path, params: params, reason: "csrf",
                        sign_in: create_console_user(admin: true), csrf: false)
    end
  end

  # Like assert_admin_only_api for console endpoints that opt in with
  # allow_active_users: anonymous, disabled, pending, bearer, and CSRF-less
  # writes are refused.
  def assert_active_user_api(method, path, params: nil)
    assert_equal :session, path_credential(path), "allow_active_users exists only in the console API"
    assert_api_denials(method, path, params: params, credential: :session)
    unless %w[get head].include?(method.to_s)
      assert_api_status(:forbidden, method, path, params: params, reason: "csrf",
                        sign_in: create_console_user, csrf: false)
    end
  end

  # :session for /api/v1/console/*, :bearer for the rest of /api/v1.
  def path_credential(path)
    path = URI.parse(path.to_s).path
    flunk "#{path} is not an /api/v1 operator path" if !path.start_with?("/api/v1/") || path.start_with?("/api/v1/sandbox/")

    path.start_with?("/api/v1/console/") ? :session : :bearer
  end

  private

  def assert_api_denials(method, path, params:, credential:)
    if credential == :session
      assert_api_status(:unauthorized, method, path, params: params, reason: "session_required")
      assert_api_status(:forbidden, method, path, params: params, reason: "session_required",
                        headers: bearer_headers(ADMIN_TOKEN))
      assert_api_status(:forbidden, method, path, params: params, reason: "session_required",
                        headers: bearer_headers("iak_bogus"))
      assert_api_status(:unauthorized, method, path, params: params, reason: "account_disabled",
                        sign_in: create_console_user, csrf: true, disable: true)
      assert_api_status(:forbidden, method, path, params: params, reason: "account_pending",
                        sign_in: create_console_user(status: :pending), csrf: false)
    else
      assert_api_status(:unauthorized, method, path, params: params)
      assert_api_status(:unauthorized, method, path, params: params, headers: bearer_headers("iak_bogus"))
      assert_api_status(:unauthorized, method, path, params: params, headers: bearer_headers(DISABLED_TOKEN))
      assert_api_status(:unauthorized, method, path, params: params, message: "invalid or missing API key",
                        sign_in: create_console_user(admin: true), csrf: true)
    end
  end

  def assert_api_status(status, method, path, params: nil, headers: {}, sign_in: nil, csrf: false,
                        disable: false, reason: nil, message: nil)
    session = open_session
    session.extend(ConsoleApiAuthHelper)
    if sign_in
      session.sign_in_console(sign_in)
      headers = headers.merge(session.csrf_headers) if csrf
      sign_in.update!(status: :disabled) if disable
    end
    session.public_send(method, path, params: params, headers: headers, as: (:json if params))
    label = "#{method.upcase} #{path} (#{sign_in ? "cookie #{sign_in.status}#{" admin" if sign_in.admin?}" : headers.keys.join(",").presence || "anonymous"})"
    assert_equal Rack::Utils.status_code(status), session.response.status, "#{label}: #{session.response.body}"
    if reason
      assert_equal reason, JSON.parse(session.response.body).dig("error", "details", "reason"), label
    end
    if message
      assert_equal message, JSON.parse(session.response.body).dig("error", "message"), label
    end
  end
end
