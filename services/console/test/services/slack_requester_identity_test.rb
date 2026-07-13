require "test_helper"

class SlackRequesterIdentityTest < ActiveSupport::TestCase
  test "resolves a verified GitHub handle from the requester's labeled Slack profile field" do
    response = Struct.new(:body).new({
      ok: true,
      profile: { fields: { "XfGithub" => { label: "GitHub", value: "https://github.com/ada" } } }
    }.to_json)
    http = Object.new
    http.define_singleton_method(:request) { |_request| response }

    Net::HTTP.stub(:start, ->(*_args, **_options, &block) { block.call(http) }) do
      result = SlackRequesterIdentity.new(token: "xoxb-test", api_url: "https://slack.test/api").resolve("UADA")

      assert_equal "@ada", result.handle
      assert_equal 'Slack profile custom field "GitHub"', result.source
    end
  end
end
