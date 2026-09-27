require "test_helper"

class Console::IntegrationsControllerTest < ActionDispatch::IntegrationTest
  test "redirects to login when not signed in" do
    get console_integrations_url
    assert_redirected_to login_path
  end

  test "chat navigation is hidden when console chat is disabled" do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    with_env("CENTAUR_CONSOLE_CHAT_ENABLED" => "false") do
      get console_integrations_url
      assert_response :ok
      assert_select "a[aria-label=?]", "New chat", count: 0
      assert_select ".console-thread-group-title", text: /Chats/, count: 0
    end
  end

  test "a non-admin sees enabled apps with their start links, logos, and no disabled apps" do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    get console_integrations_url
    assert_response :ok

    # Enabled apps show up with their consent start links.
    %w[google slack github].each do |slug|
      assert_select "a[href=?]", "http://www.example.com/oauth/#{slug}/start"
    end
    # Disabled apps are hidden.
    assert_no_match "google-disabled", response.body

    # Known providers render a brand logo (inline SVG).
    assert_select "svg path[fill='#4285F4']" # Google
    assert_select "svg path[fill='#E01E5A']" # Slack
  end

  test "an app already connected under the user's email shows Reconnect and its status" do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    credential = BrokerCredential.create!(
      oauth_app: oauth_apps(:acme_google),
      foreign_id: "google-google-member-sub",
      name: "Google – Member",
      token_endpoint: "https://oauth2.googleapis.com/token",
      client_id: "google-client-id",
      provider_subject: "member-sub",
      provider_email: users(:member_user).email,
      external_user_key: "member-key"
    )

    get console_integrations_url
    assert_response :ok
    assert_select "a.btn-secondary[href=?]", "http://www.example.com/oauth/google/start", text: "Reconnect"
    assert_match "Connected", response.body
    # The other apps are still unconnected.
    assert_select "a.btn-primary[href=?]", "http://www.example.com/oauth/slack/start", text: "Connect"

    # A dead credential asks the user to reconnect rather than claiming success.
    credential.update!(dead: true, dead_reason: "invalid_grant")
    get console_integrations_url
    assert_match "Needs reconnecting", response.body
  end

  test "a credential the user minted shows connected even when the provider email differs" do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    BrokerCredential.create!(
      oauth_app: oauth_apps(:acme_google),
      foreign_id: "google-google-personal-sub",
      name: "Google – Personal",
      token_endpoint: "https://oauth2.googleapis.com/token",
      client_id: "google-client-id",
      provider_subject: "personal-sub",
      provider_email: "personal@gmail.example",
      external_user_key: "personal-key",
      created_by: users(:member_user)
    )

    get console_integrations_url
    assert_response :ok
    assert_select "a.btn-secondary[href=?]", "http://www.example.com/oauth/google/start", text: "Reconnect"
  end

  test "a Slack credential without search scope asks the user to reconnect" do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    credential = BrokerCredential.create!(
      oauth_app: oauth_apps(:acme_slack),
      foreign_id: "slack-slack-member-sub",
      name: "Slack – Member",
      token_endpoint: "https://slack.com/api/oauth.v2.access",
      provider_subject: "U-MEMBER",
      provider_email: users(:member_user).email,
      external_user_key: "slack-member-key",
      scopes: %w[chat:write]
    )

    get console_integrations_url
    assert_response :ok
    assert_select "a.btn-secondary[href=?]", "http://www.example.com/oauth/slack/start", text: "Reconnect"
    assert_match "Needs reconnecting", response.body

    credential.update!(scopes: %w[chat:write search:read])
    get console_integrations_url
    assert_match "Connected", response.body
  end

  test "a credential minted for someone else's email does not mark the app connected" do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    BrokerCredential.create!(
      oauth_app: oauth_apps(:acme_google),
      foreign_id: "google-google-other-sub",
      name: "Google – Other",
      token_endpoint: "https://oauth2.googleapis.com/token",
      client_id: "google-client-id",
      provider_subject: "other-sub",
      provider_email: users(:acme_admin).email,
      external_user_key: "other-key"
    )

    get console_integrations_url
    assert_response :ok
    assert_select "a.btn-primary[href=?]", "http://www.example.com/oauth/google/start", text: "Connect"
    assert_no_match "Reconnect", response.body
  end

  test "an integration that is not always available discloses DM-only use" do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    get console_integrations_url
    assert_response :ok
    assert_match "Used only in your DMs with the bot", response.body
    assert_no_match "Used automatically whenever you ask the bot", response.body
  end

  test "an always-available integration discloses automatic use" do
    oauth_apps(:acme_google).update!(client_secret: "shh", always_available: true)
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    get console_integrations_url
    assert_response :ok
    assert_match "Used automatically whenever you ask the bot", response.body
    # The other apps still disclose DM-only use.
    assert_match "Used only in your DMs with the bot", response.body
  end

  test "an admin sees the same page" do
    post login_url, params: { email: users(:acme_admin).email, password: "password123456" }

    get console_integrations_url
    assert_response :ok
    assert_select "a[href=?]", "http://www.example.com/oauth/google/start"
  end
end
