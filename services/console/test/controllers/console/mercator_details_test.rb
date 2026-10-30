require "test_helper"

class Console::MercatorDetailsTest < ActionDispatch::IntegrationTest
  setup do
    user = users(:acme_admin)
    post login_url, params: { email: user.email, password: "password123456" }
    @provider = Oauth::Providers.fetch("mercator")
    @app = OauthApp.create!(@provider.preset.merge(provider: "mercator", client_id: "id", client_secret: "synthetic", created_by: user))
    @credential = BrokerCredential.create!(oauth_app: @app, provider_subject: "0x#{'ab' * 20}",
      token_endpoint: @provider.token_endpoint, access_token: "synthetic", expires_at: 1.hour.from_now)
  end

  test "claim state renders only on the app page and failures keep the link" do
    [ true, false, nil ].each do |claimed|
      http = expect_http_call(status: claimed.nil? ? 500 : 200, body: { claimed: claimed, wallet_address: @credential.provider_subject }.to_json)
      client = HttpClient.new(http: http)
      HttpClient.stub(:new, client) { get console_oauth_app_path(@app.oid) }
      assert_response :ok
      assert_select "button[disabled]", text: "MACH already claimed", count: claimed == true ? 1 : 0
      assert_select "a", text: "Claim MACH", count: claimed == true ? 0 : 1
      assert_select "a", text: "Manage wallet ↗"
      http.verify
    end
    @provider.stub(:details_for, ->(*) { flunk "No details on Integrations" }) { get console_integrations_path }
    assert_response :ok
  end

  test "expired or dead credentials never fetch details or refresh" do
    [ { expires_at: 1.minute.ago }, { expires_at: 1.hour.from_now, dead: true } ].each do |attrs|
      @credential.update!(attrs)
      @provider.stub(:details_for, ->(*) { flunk "No details for expired or dead credentials" }) do
        get console_oauth_app_path(@app.oid)
      end
      assert_response :ok
      assert_select "a", text: "Claim MACH"
    end
  end

  test "unexpected provider failure does not break the page" do
    @provider.stub(:details_for, ->(*) { raise TypeError }) { get console_oauth_app_path(@app.oid) }
    assert_response :ok
    assert_select "a", text: "Claim MACH"
  end
end
