require "test_helper"

class SlackCrewClientTest < ActiveSupport::TestCase
  test "uses admin bearer token and implements operator and self endpoints" do
    calls = []
    transport = lambda do |**request|
      calls << request
      HttpClient::Response.new(status: 200, body: { ok: true }.to_json)
    end
    client = SlackCrewClient.new(base_url: "https://crew.internal", token: "admin-secret", http: transport)

    client.list
    client.create(id: "alpha", name: "Alpha", crew_id: "engineer", description: "Helper")
    client.update("alpha/beta", name: "New", paused: true)
    client.self_get("A/1")
    client.self_update("A/1", name: "Self")

    assert_equal %i[get post post get post], calls.map { |call| call[:method] }
    assert_equal "/api/slack/crew/alpha%2Fbeta/manage", URI(calls[2][:url]).path
    assert_equal "/api/slack/crew/by-app/A%2F1/manage", URI(calls[3][:url]).path
    assert calls.all? { |call| call[:headers]["Authorization"] == "Bearer admin-secret" }
  end

  test "reports status without exposing raw transport failures" do
    response = ->(**) { HttpClient::Response.new(status: 503, body: "not json secret-value") }
    error = assert_raises(SlackCrewClient::Error) do
      SlackCrewClient.new(base_url: "https://crew.internal", token: "token", http: response).list
    end
    assert_equal 503, error.status
    assert_equal "Crew service returned an invalid response", error.message

    failure = ->(**) { raise "socket failed with token-secret" }
    error = assert_raises(SlackCrewClient::Error) do
      SlackCrewClient.new(base_url: "https://crew.internal", token: "token", http: failure).list
    end
    assert_equal "Crew service is unavailable", error.message
    assert_no_match(/secret/, error.message)
  end
end
