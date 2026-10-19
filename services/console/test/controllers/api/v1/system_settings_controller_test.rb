require "test_helper"

class SystemSettingsControllerTest < ActionDispatch::IntegrationTest
  ACME_TOKEN = "iak_acme-ci-token".freeze

  test "rejects requests without an Authorization header" do
    get api_v1_system_settings_url
    assert_response :unauthorized
  end

  test "GET returns the allowed Slack Connect workspaces" do
    system_settings(:default).update!(slack_external_team_ids: %w[T0PARTNER1 T0PARTNER2])

    get api_v1_system_settings_url, headers: { "Authorization" => "Bearer #{ACME_TOKEN}" }

    assert_response :ok
    assert_equal "no-store", response.headers["Cache-Control"]
    assert_equal({ "slack_external_team_ids" => %w[T0PARTNER1 T0PARTNER2] }, JSON.parse(response.body).fetch("data"))
  end
end
