require "test_helper"

# The OAuth apps and Integrations pages are served by the SPA; their legacy
# HTML form and preset routes are gone.
class OauthAppsSpaRoutesTest < ActionDispatch::IntegrationTest
  HTML = { "Accept" => "text/html,application/xhtml+xml" }.freeze

  setup do
    @dir = Dir.mktmpdir
    @index = File.join(@dir, "index.html")
    File.write(@index, "<!doctype html><div id=\"root\"></div>")
    @previous_index_path = SpaController.index_path
    SpaController.index_path = @index
  end

  teardown do
    SpaController.index_path = @previous_index_path
    FileUtils.remove_entry(@dir)
  end

  test "the apps and integrations pages are served by the SPA" do
    app = oauth_apps(:acme_google)
    [ users(:acme_admin), users(:member_user) ].each do |user|
      sign_in_console(user)
      [ "/console/oauth_apps", "/console/oauth_apps/new", "/console/oauth_apps/#{app.oid}",
        "/console/oauth_apps/#{app.oid}/edit", "/console/integrations" ].each do |path|
        get path, headers: HTML
        assert_response :ok, path
        assert_includes response.body, "<div id=\"root\">", path
      end
    end
  end

  test "legacy oauth app form and preset routes are gone" do
    sign_in_console(users(:acme_admin))
    app = oauth_apps(:acme_google)
    post "/console/oauth_apps", params: { oauth_app: { slug: "legacy", provider: "google" } }
    assert_response :not_found
    patch "/console/oauth_apps/#{app.oid}", params: { oauth_app: { description: "legacy" } }
    assert_response :not_found
    post "/console/oauth_apps/presets/mercator"
    assert_response :not_found
    assert_not OauthApp.exists?(slug: "legacy")
    assert_not OauthApp.exists?(slug: "mercator")
    assert_equal "Acme Google integration", app.reload.description
  end

  test "path helpers used by redirects still point at the SPA pages" do
    app = oauth_apps(:acme_google)
    assert_equal "/console/integrations", console_integrations_path
    assert_equal "/console/oauth_apps", console_oauth_apps_path
    assert_equal "/console/oauth_apps/#{app.oid}", console_oauth_app_path(app.oid)
  end
end
