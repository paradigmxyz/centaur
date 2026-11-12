require "test_helper"

# The console session cookie is Secure whenever the request came over HTTPS
# (directly or via a TLS-terminating ingress), and stays usable over plain
# HTTP for deployments without TLS.
class SecureSessionCookieTest < ActionDispatch::IntegrationTest
  def login_cookie_header(**request_options)
    get login_url, **request_options
    post login_url, params: { email: users(:acme_admin).email, password: "password123456" }, **request_options
    Array(response.headers["Set-Cookie"]).flat_map { |h| h.split("\n") }.find { |c| c.start_with?("_iron_control_session=") }
  end

  test "the session cookie is Secure over HTTPS" do
    https!
    cookie = login_cookie_header
    assert cookie, "expected a session cookie"
    assert_match(/;\s*secure/i, cookie)
    assert_match(/;\s*httponly/i, cookie)
  end

  test "the session cookie is Secure behind a TLS-terminating proxy" do
    cookie = login_cookie_header(headers: { "X-Forwarded-Proto" => "https" })
    assert cookie, "expected a session cookie"
    assert_match(/;\s*secure/i, cookie)
  end

  test "the session cookie still works over plain HTTP" do
    cookie = login_cookie_header
    assert cookie, "expected a session cookie"
    assert_no_match(/;\s*secure/i, cookie)
    get "/api/v1/me"
    assert_response :ok
  end

  test "an https public URL forces Secure on every request" do
    assert SecureSessionCookie.https_public_url?("https://console.example.test")
    assert_not SecureSessionCookie.https_public_url?("http://console.internal")
    assert_not SecureSessionCookie.https_public_url?(nil)

    seen = nil
    app = SecureSessionCookie.new(->(env) { seen = env[Rack::RACK_SESSION_OPTIONS][:secure]; [ 200, {}, [] ] }, always: true)
    app.call(Rack::MockRequest.env_for("http://console.example.test/", Rack::RACK_SESSION_OPTIONS => {}))
    assert_equal true, seen
  end
end
