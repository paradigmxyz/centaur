require "test_helper"

class HttpClientTest < ActiveSupport::TestCase
  FakeResponse = Struct.new(:code, :body)

  def expect_transport_call(response)
    expect_http_call(status: response.status, body: response.body, headers: response.headers) { |request| yield request if block_given? }
  end

  test "serializes JSON requests and parses JSON responses" do
    http = expect_transport_call(HttpClient::Response.new(status: 200, body: { "ok" => true }.to_json)) do |request|
      assert_equal :post, request[:method]
      assert_equal "https://api.test/widgets?existing=1&page=2", request[:url]
      assert_equal({ "name" => "demo" }, JSON.parse(request[:body]))
      assert_equal "application/json", request[:headers]["Accept"]
      assert_equal "application/json", request[:headers]["Content-Type"]
      assert_equal "Bearer token", request[:headers]["Authorization"]
      assert_equal 5, request[:timeout]
    end
    client = HttpClient.new(http: http, open_timeout: 3, read_timeout: 5)

    response = client.post(
      "https://api.test/widgets?existing=1",
      params: { page: 2, empty: nil },
      json: { name: "demo" },
      headers: { "Authorization" => "Bearer token" }
    )

    assert_equal({ "ok" => true }, response.json)
    http.verify
  end

  test "serializes form requests" do
    http = expect_transport_call(HttpClient::Response.new(status: 200, body: "{}")) do |request|
      assert_equal "grant_type=refresh_token", request[:body]
      assert_equal "application/x-www-form-urlencoded", request[:headers]["Content-Type"]
    end
    client = HttpClient.new(http: http)

    client.post("https://api.test/token", form: { "grant_type" => "refresh_token" })

    http.verify
  end

  test "omits JSON request bodies when JSON is nil" do
    http = expect_transport_call(HttpClient::Response.new(status: 204, body: "")) do |request|
      assert_nil request[:body]
      assert_nil request[:headers]["Content-Type"]
    end
    client = HttpClient.new(http: http)

    client.request(method: :delete, url: "https://api.test/widgets/1", json: nil)

    http.verify
  end

  test "supports custom accept headers" do
    http = expect_transport_call(HttpClient::Response.new(status: 200, body: "{}")) do |request|
      assert_equal "application/vnd.github+json", request[:headers]["Accept"]
    end
    client = HttpClient.new(http: http)

    client.get("https://api.test/user", headers: { "Accept" => "application/vnd.github+json" })

    http.verify
  end

  test "uses default timeouts for net http requests" do
    http = Minitest::Mock.new
    http.expect(:use_ssl=, true, [ true ])
    http.expect(:open_timeout=, HttpClient::DEFAULT_OPEN_TIMEOUT, [ HttpClient::DEFAULT_OPEN_TIMEOUT ])
    http.expect(:read_timeout=, HttpClient::DEFAULT_READ_TIMEOUT, [ HttpClient::DEFAULT_READ_TIMEOUT ])
    http.expect(:request, FakeResponse.new("200", "{}")) do |_request|
      assert true
    end

    Net::HTTP.stub(:new, ->(_host, _port) { http }) do
      HttpClient.new.get("https://api.test/user")
    end

    http.verify
  end

  test "preserves caller content type for encoded form requests" do
    http = Minitest::Mock.new
    http.expect(:use_ssl=, true, [ true ])
    http.expect(:open_timeout=, HttpClient::DEFAULT_OPEN_TIMEOUT, [ HttpClient::DEFAULT_OPEN_TIMEOUT ])
    http.expect(:read_timeout=, HttpClient::DEFAULT_READ_TIMEOUT, [ HttpClient::DEFAULT_READ_TIMEOUT ])
    http.expect(:request, FakeResponse.new("200", "{}")) do |request|
      assert_equal "application/custom-form", request["Content-Type"]
      true
    end

    Net::HTTP.stub(:new, ->(_host, _port) { http }) do
      HttpClient.new.post(
        "https://api.test/token",
        form: { "grant_type" => "refresh_token" },
        headers: { "Content-Type" => "application/custom-form" }
      )
    end

    http.verify
  end
end
