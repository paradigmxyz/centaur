require "test_helper"

# The manifest and service worker are fetched by the browser outside an
# authenticated page load, so they must be served without a console session
# (Rails::PwaController, not ApplicationController).
class PwaTest < ActionDispatch::IntegrationTest
  test "manifest is served without a console session" do
    get pwa_manifest_url(format: :json)
    assert_response :ok

    manifest = JSON.parse(response.body)
    assert_equal "Centaur Console", manifest["name"]
    assert_equal "standalone", manifest["display"]
    assert manifest["icons"].any? { |icon| icon["sizes"] == "512x512" }
    assert_equal "/", manifest.dig("file_handlers", 0, "action")
    assert_equal "web+centaur", manifest.dig("protocol_handlers", 0, "protocol")
    assert_equal %w[/console/threads /console/workflows /console/integrations],
                 manifest["shortcuts"].map { |shortcut| shortcut["url"] }
  end

  test "launch maps web+centaur targets onto in-app paths" do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    get launch_url(target: "web+centaur://console/workflows")
    assert_redirected_to "/console/workflows"

    # Anything that is not a plain in-app path falls back to the root: missing
    # scheme, empty target, dots, queries, or protocol-relative smuggling.
    [
      "console/workflows",
      "",
      "web+centaur://../etc/passwd",
      "web+centaur://console/threads?x=1",
      "web+centaur:////evil.example"
    ].each do |target|
      get launch_url(target: target)
      assert_redirected_to root_path, "expected fallback for #{target.inspect}"
    end
  end

  test "launch requires a console session" do
    get launch_url(target: "web+centaur://console/threads")
    assert_redirected_to login_path
  end

  test "service worker is served without a console session" do
    get pwa_service_worker_url(format: :js)
    assert_response :ok
    assert_match "OFFLINE_URL", response.body
  end

  test "console layout renders the install entry point" do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    get console_integrations_url
    assert_response :ok
    assert_select "[data-controller=pwa-install]"
  end
end
