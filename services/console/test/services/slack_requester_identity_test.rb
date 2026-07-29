require "test_helper"

class SlackRequesterIdentityTest < ActiveSupport::TestCase
  test "resolves a verified GitHub handle from the requester's labeled Slack profile field" do
    api = Minitest::Mock.new
    api.expect(:get, HttpClient::Response.new(status: 200, body: {
      ok: true,
      profile: { fields: { "XfGithub" => { label: "GitHub", value: "https://github.com/ada" } } }
    }.to_json), [ "https://slack.test/api/users.profile.get" ], params: { user: "UADA", include_labels: "true" },
                                                            headers: { "Authorization" => "Bearer xoxb-test" })

    result = SlackRequesterIdentity.new(token: "xoxb-test", api_url: "https://slack.test/api", api: api).resolve("UADA")

    assert_equal "@ada", result.handle
    assert_equal 'Slack profile custom field "GitHub"', result.source
    api.verify
  end
end
