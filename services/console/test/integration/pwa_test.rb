require "test_helper"

# The manifest and service worker are fetched by the browser outside an
# authenticated page load, so they must be served without a console session
# (PwaController, not ApplicationController), as static files.
class PwaTest < ActionDispatch::IntegrationTest
  test "manifest is served without a console session" do
    get pwa_manifest_url(format: :json)
    assert_response :ok

    assert_equal "application/manifest+json", response.media_type
    assert_equal "no-cache", response.headers["Cache-Control"]
    assert_nil response.headers["Set-Cookie"]
    manifest = JSON.parse(response.body)
    assert_equal "Centaur Console", manifest["name"]
    assert_equal "standalone", manifest["display"]
    assert manifest["icons"].any? { |icon| icon["sizes"] == "512x512" }
    assert_equal "/", manifest.dig("file_handlers", 0, "action")
    assert_equal "web+centaur", manifest.dig("protocol_handlers", 0, "protocol")
    assert_equal %w[/console/workflows /console/integrations],
                 manifest["shortcuts"].map { |shortcut| shortcut["url"] }
  end

  test "launch maps web+centaur targets onto in-app paths" do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    get launch_url(target: "web+centaur://console/workflows")
    assert_redirected_to "/console/workflows"
    get launch_url(target: "web+centaur://console")
    assert_redirected_to "/console"
    get launch_url(target: "web+centaur:///console//integrations/")
    assert_redirected_to "/console/integrations"

    # Anything that is not a plain in-app path falls back to the root: missing
    # scheme, empty target, dots, queries, or protocol-relative smuggling.
    [
      "console/workflows",
      "",
      "web+centaur://../etc/passwd",
      "web+centaur://console/workflows?x=1",
      "web+centaur:////evil.example",
      "web+centaur://evil.example/console",
      "web+centaur://api/v1/me",
      "web+centaur://logout",
      "web+centaur://oauth/google/start",
      "web+centaur://console/..%2f..%2fapi",
      "web+centaur://console\\evil"
    ].each do |target|
      get launch_url(target: target)
      assert_redirected_to root_path, "expected fallback for #{target.inspect}"
    end
  end

  test "launch requires a console session and resumes after sign-in" do
    get launch_url(target: "web+centaur://console/integrations")
    assert_redirected_to login_path

    post login_url, params: { email: users(:member_user).email, password: "password123456" }
    assert_redirected_to "/launch?target=web%2Bcentaur%3A%2F%2Fconsole%2Fintegrations"
  end

  test "launch sends pending users to the holding page" do
    post login_url, params: { email: users(:pending_user).email, password: "password123456" }
    get launch_url(target: "web+centaur://console/integrations")
    assert_redirected_to pending_path
  end

  test "service worker is served without a console session" do
    get pwa_service_worker_url(format: :js)
    assert_response :ok
    assert_equal "text/javascript", response.media_type
    assert_equal "no-cache", response.headers["Cache-Control"]
    assert_match "OFFLINE_URL", response.body
    # Never caches the SPA shell or the API.
    assert_match %r{url.pathname.startsWith\("/spa/assets/"\)}, response.body
    assert_no_match %r{startsWith\("/api}, response.body
  end

  test "both are served with or without the format extension" do
    get "/manifest"
    assert_response :ok
    get "/service-worker"
    assert_response :ok
  end
end
