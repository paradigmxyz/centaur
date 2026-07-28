require "test_helper"

class HttpClientTest < ActiveSupport::TestCase
  FakeResponse = Struct.new(:code, :body)

  class StubHTTP
    attr_reader :requests

    def initialize(response)
      @response = response
      @requests = []
    end

    def call(method:, url:, body:, headers:, timeout:)
      @requests << { method: method, url: url, body: body, headers: headers, timeout: timeout }
      @response
    end
  end

  class FakeNetHTTP
    attr_reader :open_timeout, :read_timeout, :captured_request

    def initialize(response)
      @response = response
    end

    attr_writer :use_ssl

    def open_timeout=(value)
      @open_timeout = value
    end

    def read_timeout=(value)
      @read_timeout = value
    end

    def request(request)
      @captured_request = request
      @response
    end
  end

  test "serializes JSON requests and parses JSON responses" do
    http = StubHTTP.new(HttpClient::Response.new(status: 200, body: { "ok" => true }.to_json))
    client = HttpClient.new(http: http, open_timeout: 3, read_timeout: 5)

    response = client.post(
      "https://api.test/widgets?existing=1",
      params: { page: 2, empty: nil },
      json: { name: "demo" },
      headers: { "Authorization" => "Bearer token" }
    )

    assert_equal({ "ok" => true }, response.json)
    request = http.requests.first
    assert_equal :post, request[:method]
    assert_equal "https://api.test/widgets?existing=1&page=2", request[:url]
    assert_equal({ "name" => "demo" }, JSON.parse(request[:body]))
    assert_equal "application/json", request[:headers]["Accept"]
    assert_equal "application/json", request[:headers]["Content-Type"]
    assert_equal "Bearer token", request[:headers]["Authorization"]
    assert_equal 5, request[:timeout]
  end

  test "serializes form requests" do
    http = StubHTTP.new(HttpClient::Response.new(status: 200, body: "{}"))
    client = HttpClient.new(http: http)

    client.post("https://api.test/token", form: { "grant_type" => "refresh_token" })

    request = http.requests.first
    assert_equal "grant_type=refresh_token", request[:body]
    assert_equal "application/x-www-form-urlencoded", request[:headers]["Content-Type"]
  end

  test "omits JSON request bodies when JSON is nil" do
    http = StubHTTP.new(HttpClient::Response.new(status: 204, body: ""))
    client = HttpClient.new(http: http)

    client.request(method: :delete, url: "https://api.test/widgets/1", json: nil)

    request = http.requests.first
    assert_nil request[:body]
    assert_nil request[:headers]["Content-Type"]
  end

  test "supports custom accept headers" do
    http = StubHTTP.new(HttpClient::Response.new(status: 200, body: "{}"))
    client = HttpClient.new(http: http)

    client.get("https://api.test/user", headers: { "Accept" => "application/vnd.github+json" })

    assert_equal "application/vnd.github+json", http.requests.first[:headers]["Accept"]
  end

  test "uses default timeouts for net http requests" do
    http = FakeNetHTTP.new(FakeResponse.new("200", "{}"))

    Net::HTTP.stub(:new, ->(_host, _port) { http }) do
      HttpClient.new.get("https://api.test/user")
    end

    assert_equal HttpClient::DEFAULT_OPEN_TIMEOUT, http.open_timeout
    assert_equal HttpClient::DEFAULT_READ_TIMEOUT, http.read_timeout
  end

  test "preserves caller content type for encoded form requests" do
    http = FakeNetHTTP.new(FakeResponse.new("200", "{}"))

    Net::HTTP.stub(:new, ->(_host, _port) { http }) do
      HttpClient.new.post(
        "https://api.test/token",
        form: { "grant_type" => "refresh_token" },
        headers: { "Content-Type" => "application/custom-form" }
      )
    end

    assert_equal "application/custom-form", http.captured_request["Content-Type"]
  end
end
