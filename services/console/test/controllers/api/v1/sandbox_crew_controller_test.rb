require "test_helper"

class Api::V1::SandboxCrewControllerTest < ActionDispatch::IntegrationTest
  setup do
    @proxy = proxies(:acme_proxy)
    @calls = []
    calls = @calls
    client = Object.new
    client.define_singleton_method(:self_get) { |app| calls << [ app ]; { app_id: app } }
    client.define_singleton_method(:self_update) { |app, fields| calls << [ app, fields ]; fields.merge(app_id: app) }
    Api::V1::Sandbox::CrewController.client_factory = -> { client }
    # A disposable table with the actual API columns. No api-rs migrations or
    # shared session database are required by the Console test suite.
    CrewSession.table_name = "crew_test_sessions"
    @db = CrewSession.lease_connection
    @db.execute("CREATE TEMPORARY TABLE crew_test_sessions (thread_key text PRIMARY KEY, sandbox_id text, iron_control_principal text)")
    insert_session("slack:T123:A111:C123:123.456", @proxy.name, @proxy.principal.oid)
    insert_session("slack:T123:A222:C123:123.456", "other-sandbox", @proxy.principal.oid)
  end

  teardown do
    @db&.execute("DROP TABLE IF EXISTS crew_test_sessions")
    CrewSession.table_name = "sessions"
    Api::V1::Sandbox::CrewController.client_factory = -> { SlackCrewClient.new }
  end

  test "authenticates sandbox and never accepts the console cookie as bot authority" do
    post login_url, params: { email: users(:acme_admin).email, password: "password123456" }
    get "/api/v1/sandbox/crew/me"
    assert_response :unauthorized
    assert_empty @calls
  end

  test "two bots in the same channel principal can only edit their own app" do
    with_token do |headers|
      get "/api/v1/sandbox/crew/me", headers: headers
      assert_response :ok
      assert_equal "no-store", response.headers["Cache-Control"]
      patch "/api/v1/sandbox/crew/me", params: { data: { name: "Renamed", description: "My description" } }, headers: headers, as: :json
      assert_response :ok
    end
    @proxy.update!(name: "other-sandbox")
    with_token do |headers|
      get "/api/v1/sandbox/crew/me", headers: headers
      assert_response :ok
    end
    assert_equal [ [ "A111" ], [ "A111", { "name" => "Renamed", "description" => "My description" } ], [ "A222" ] ], @calls
  end

  test "rejects identity selectors and administrative fields without forwarding" do
    with_token do |headers|
      %w[id app_id crew_id paused scopes token].each do |field|
        patch "/api/v1/sandbox/crew/me", params: { data: { name: "Other", field => "A222" } }, headers: headers, as: :json
        assert_response :bad_request
      end
      patch "/api/v1/sandbox/crew/me?app_id=A222", params: { data: { name: "Other" } }, headers: headers, as: :json
      assert_response :bad_request
      patch "/api/v1/sandbox/crew/me", params: { data: { name: "Other" }, app_id: "A222" }, headers: headers, as: :json
      assert_response :bad_request
    end
    assert_empty @calls
  end

  test "released sandbox or principal mismatch cannot fall back to another bot in the channel" do
    with_token do |headers|
      @db.execute("UPDATE crew_test_sessions SET sandbox_id = NULL WHERE thread_key LIKE '%A111:%'")
      get "/api/v1/sandbox/crew/me", headers: headers
      assert_response :forbidden
      @db.execute("UPDATE crew_test_sessions SET sandbox_id = #{@db.quote(@proxy.name)}, iron_control_principal = 'someone-else'")
      get "/api/v1/sandbox/crew/me", headers: headers
      assert_response :forbidden
    end
    assert_empty @calls
  end

  test "legacy or ambiguous assignments fail closed" do
    with_token do |headers|
      @db.execute("UPDATE crew_test_sessions SET thread_key = 'slack:C123:123.456' WHERE thread_key LIKE '%A111:%'")
      get "/api/v1/sandbox/crew/me", headers: headers
      assert_response :forbidden
      insert_session("slack:T123:A333:C123:999.456", @proxy.name, @proxy.principal.oid)
      get "/api/v1/sandbox/crew/me", headers: headers
      assert_response :forbidden
    end
    assert_empty @calls
  end

  test "reassigned proxy invalidates its previous token" do
    with_token do |headers|
      @proxy.update!(principal: principals(:globex_user))
      get "/api/v1/sandbox/crew/me", headers: headers
      assert_response :unauthorized
    end
    assert_empty @calls
  end

  private

  def insert_session(key, sandbox, principal)
    @db.execute("INSERT INTO crew_test_sessions VALUES (#{[ key, sandbox, principal ].map { |v| @db.quote(v) }.join(', ')})")
  end

  def with_token
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      yield "Authorization" => "Bearer #{SandboxEntitlements::Jwt.encode_for_proxy(@proxy)}"
    end
  end
end
