require "test_helper"

class Console::OauthPresetsControllerTest < ActionDispatch::IntegrationTest
  setup do
    @admin = users(:acme_admin)
    post login_url, params: { email: @admin.email, password: "password123456" }
  end

  teardown do
    Console::OauthAppsController.registration_client_factory = -> { Oauth::DynamicRegistration.new }
  end

  def registration_client(&block)
    Console::OauthAppsController.registration_client_factory = -> {
      Oauth::DynamicRegistration.new(http: HttpClient.new(http: block))
    }
  end

  test "preset registers once even with existing manual credentials and then shows manage" do
    BrokerCredential.create!(token_endpoint: "https://mercator.sh/oauth/token", client_id: "manual")
    registration_client do |**request|
      assert_equal [ "http://www.example.com/oauth/mercator/callback" ], JSON.parse(request[:body])["redirect_uris"]
      HttpClient::Response.new(status: 201, body: { client_id: "id", client_secret: "synthetic" }.to_json)
    end
    assert_difference("OauthApp.count", 1) { post console_oauth_app_preset_path(provider: "mercator") }
    app = OauthApp.find_by!(slug: "mercator")
    assert app.shared?
    assert_redirected_to oauth_start_path(slug: "mercator")
    registration_client { |**| flunk "Must reuse existing app" }
    assert_no_difference("OauthApp.count") { post console_oauth_app_preset_path(provider: "mercator") }
    BrokerCredential.create!(oauth_app: app, provider_subject: "0x#{'ab' * 20}", token_endpoint: app.provider_strategy.token_endpoint)
    get console_integrations_path
    assert_select "a[href=?]", console_oauth_app_path(app.oid), text: "Manage"
    assert_select "form[action*='/presets/mercator']", count: 0
  end

  test "registration scopes use spaces even when consent scopes use commas" do
    provider = Oauth::Providers.fetch("mercator")
    preset = provider.preset.merge(allowed_scopes: %w[read write])
    registration_client do |**request|
      assert_equal "read write", JSON.parse(request[:body])["scope"]
      HttpClient::Response.new(status: 201, body: { client_id: "id", client_secret: "synthetic" }.to_json)
    end
    provider.stub(:preset, preset) do
      provider.stub(:scope_separator, ",") do
        post console_oauth_app_preset_path(provider: "mercator")
      end
    end
    assert_redirected_to oauth_start_path(slug: "mercator")
  end

  test "preset reuses the winner of a concurrent unique slug conflict" do
    provider = Oauth::Providers::Mercator.new
    winner = OauthApp.create!(provider.preset.merge(provider: "mercator", client_id: "winner", client_secret: "synthetic", created_by: @admin))
    lookups = 0
    lookup = ->(*) { lookups += 1; lookups == 1 ? nil : winner }
    registration_client { |**| HttpClient::Response.new(status: 201, body: { client_id: "loser", client_secret: "synthetic" }.to_json) }
    OauthApp.stub(:find_by, lookup) do
      OauthApp.stub(:create!, ->(*) { raise ActiveRecord::RecordNotUnique }) do
        post console_oauth_app_preset_path(provider: "mercator")
      end
    end
    assert_redirected_to oauth_start_path(slug: "mercator")
    assert_equal "winner", winner.reload.client_id
  end

  test "registration failure leaves no app and returns to integrations" do
    registration_client { |**| raise Timeout::Error }
    assert_no_difference("OauthApp.count") { post console_oauth_app_preset_path(provider: "mercator") }
    assert_redirected_to console_integrations_path
    assert flash[:alert].present?
  end

  test "non admins and unsupported providers cannot register" do
    registration_client { |**| flunk "Must not register" }
    post console_oauth_app_preset_path(provider: "google")
    assert_response :not_found
    delete logout_url
    post login_url, params: { email: users(:member_user).email, password: "password123456" }
    post console_oauth_app_preset_path(provider: "mercator")
    assert_redirected_to console_integrations_path
  end
end
