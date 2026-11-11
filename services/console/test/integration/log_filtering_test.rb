require "test_helper"
require "base64"
require "digest"

# Real log capture: OAuth codes (and other filtered names) must not reach the
# "Started", "Parameters", or "Redirected to" lines, including when the name
# is percent-encoded or nested, which Rails' own name filters miss on the
# raw query string (ConsoleLogFilter).
class LogFilteringTest < ActionDispatch::IntegrationTest
  MARKERS = %w[SYNTHETIC_MARKER_1 SYNTHETIC_MARKER_2 SYNTHETIC_MARKER_3 SYNTHETIC_MARKER_4 SYNTHETIC_MARKER_5].freeze

  setup do
    @io = StringIO.new
    @capture = ActiveSupport::Logger.new(@io)
    @capture.level = :debug
    Rails.logger.broadcast_to(@capture)
    @saved_mcp_url = ENV["CENTAUR_MCP_PUBLIC_URL"]
    ENV["CENTAUR_MCP_PUBLIC_URL"] = "http://localhost:3000/mcp"
  end

  teardown do
    Rails.logger.stop_broadcasting_to(@capture)
    @saved_mcp_url.nil? ? ENV.delete("CENTAUR_MCP_PUBLIC_URL") : ENV["CENTAUR_MCP_PUBLIC_URL"] = @saved_mcp_url
  end

  def log = @io.string

  test "percent-encoded and nested code names are filtered from the Started and Parameters lines" do
    get "/auth/unconfigured/callback?c%6fde=#{MARKERS[0]}&%63ode=#{MARKERS[1]}&data[code]=#{MARKERS[2]}" \
        "&data%5Bc%6Fde%5D=#{MARKERS[3]}&t%6Fken=#{MARKERS[4]}&state=visible-state"

    assert_match %r{Started GET "/auth/unconfigured/callback\?}, log
    assert_match(/Parameters: /, log)
    MARKERS.each { |marker| assert_not_includes log, marker }
    assert_includes log, "visible-state"

    # Filtering only touches the logged copy: the request still sees the values.
    assert_includes [ MARKERS[0], MARKERS[1] ], request.params["code"]
    assert_equal MARKERS[4], request.params["token"]
  end

  test "double-encoded names are filtered too" do
    get "/auth/unconfigured/callback?c%256fde=#{MARKERS[0]}"
    assert_match(/Started GET/, log)
    assert_not_includes log, MARKERS[0]
  end

  test "unrelated parameters stay readable" do
    get "/auth/unconfigured/callback?country_code=NZ&c%6fdex=keep-me"
    assert_includes log, "country_code=NZ"
    assert_includes log, "keep-me"
  end

  test "the MCP consent redirect logs no authorization code" do
    client = McpOauthClient.create!(
      name: "Amp", redirect_uris: [ "http://127.0.0.1:49152/callback" ],
      grant_types: McpOauthClient::DEFAULT_GRANT_TYPES, response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
      scopes: McpOauthClient::DEFAULT_SCOPES
    )
    sign_in_console(users(:acme_admin))
    get "/mcp/oauth/authorize", params: authorize_params(client)
    assert_response :ok

    post "/mcp/oauth/authorize", params: authorize_params(client).merge(decision: "approve")
    code = Rack::Utils.parse_nested_query(URI.parse(response.location).query).fetch("code")

    assert_match %r{Redirected to http://127\.0\.0\.1:49152/callback\?code=\[FILTERED\]}, log
    assert_not_includes log, code
  end

  test "a client redirect with an encoded nested code name is filtered in Redirected to" do
    redirect_uri = "http://127.0.0.1:49152/callback?x%5Bc%6fde%5D=#{MARKERS[0]}"
    client = McpOauthClient.create!(
      name: "Amp", redirect_uris: [ redirect_uri ],
      grant_types: McpOauthClient::DEFAULT_GRANT_TYPES, response_types: McpOauthClient::DEFAULT_RESPONSE_TYPES,
      scopes: McpOauthClient::DEFAULT_SCOPES
    )
    sign_in_console(users(:acme_admin))
    @io.truncate(0)
    @io.rewind
    get "/mcp/oauth/authorize", params: authorize_params(client).merge(redirect_uri: redirect_uri, scope: "admin")

    assert_response :redirect
    assert_includes response.location, MARKERS[0]
    # (The marker also appears inside the logged redirect_uri *value*: that is
    # the client's own registration, not a parameter named code.)
    redirected = log.lines.grep(/Redirected to /).join
    assert_match %r{x%5Bcode%5D=\[FILTERED\]}, redirected
    assert_not_includes redirected, MARKERS[0]
  end

  test "the token exchange logs no code or verifier" do
    post "/mcp/oauth/token", params: { grant_type: "authorization_code", client_id: "moc_none",
                                       code: MARKERS[0], code_verifier: MARKERS[1] }
    assert_match(/Parameters: /, log)
    assert_not_includes log, MARKERS[0]
    assert_not_includes log, MARKERS[1]
  end

  private

  def authorize_params(client)
    {
      response_type: "code", client_id: client.public_client_id, redirect_uri: "http://127.0.0.1:49152/callback",
      scope: "mcp:tools", state: "state-test", resource: "http://localhost:3000/mcp",
      code_challenge: Base64.urlsafe_encode64(Digest::SHA256.digest("verifier"), padding: false),
      code_challenge_method: "S256"
    }
  end
end
