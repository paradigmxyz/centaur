require "test_helper"

class Api::V1::SandboxCrewControllerTest < ActionDispatch::IntegrationTest
  setup do
    @proxy = proxies(:acme_proxy)
    @profile = CrewProfile.new(crew_id: "alpha")
    @profile.provision!({ "id" => "alpha", "name" => "Alpha", "app_id" => "A111", "team_id" => "T123" }, user: users(:acme_admin))
    @other = CrewProfile.new(crew_id: "beta", system_prompt: "Other bot instructions")
    @other.provision!({ "id" => "beta", "name" => "Beta", "app_id" => "A222", "team_id" => "T123" }, user: users(:acme_admin))
    @proxy.update!(principal: @profile.principal)
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
    insert_session("slack:T123:A222:C123:123.456", "other-sandbox", @other.principal.oid)
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

  test "two bots in the same channel can only edit their own app" do
    with_token do |headers|
      get "/api/v1/sandbox/crew/me", headers: headers
      assert_response :ok
      assert_equal "no-store", response.headers["Cache-Control"]
      patch "/api/v1/sandbox/crew/me", params: { data: { name: "Renamed", description: "My description" } }, headers: headers, as: :json
      assert_response :ok
    end
    @proxy.update!(name: "other-sandbox", principal: @other.principal)
    with_token do |headers|
      get "/api/v1/sandbox/crew/me", headers: headers
      assert_response :ok
    end
    assert_equal [ [ "A111" ], [ "A111" ], [ "A111", { "name" => "Renamed", "description" => "My description" } ], [ "A222" ] ], @calls
  end

  test "rejects identity selectors and administrative fields without forwarding" do
    with_token do |headers|
      %w[id app_id crew_id paused scopes token roles role_ids role_oids principal_id].each do |field|
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

  test "picture updates target only the sandbox app and cannot supply another app selector" do
    picture = "https://images.example.com/alpha.png"
    with_token do |headers|
      patch "/api/v1/sandbox/crew/me", params: { data: { icon_url: picture } }, headers: headers, as: :json
      assert_response :ok
      assert_equal picture, response.parsed_body["data"]["icon_url"]
      assert_equal [ [ "A111" ], [ "A111", { "icon_url" => picture } ] ], @calls
      @calls.clear
      patch "/api/v1/sandbox/crew/me", params: { data: { icon_url: picture, app_id: "A222" } }, headers: headers, as: :json
      assert_response :bad_request
      assert_empty @calls
      assert_equal "Other bot instructions", @other.reload.system_prompt
    end
  end

  test "self updates prompt skills and models with a revision but cannot change another profile" do
    with_token do |headers|
      fields = { system_prompt: "My revised prompt", skills: [ { name: "reviewing-code", description: "Reviews code when asked.", content: "Check boundaries." } ], default_models: { claude: "my-model" }, lock_version: 0 }
      patch "/api/v1/sandbox/crew/me", params: { data: fields }, headers: headers, as: :json
      assert_response :ok
      assert_equal 1, response.parsed_body["data"]["lock_version"]
      assert_equal "My revised prompt", @profile.reload.system_prompt
      assert_equal "reviewing-code", @profile.skills.first["name"]
      assert_equal "Other bot instructions", @other.reload.system_prompt
      patch "/api/v1/sandbox/crew/me", params: { data: fields.merge(system_prompt: "Stale") }, headers: headers, as: :json
      assert_response :conflict
      assert_equal "My revised prompt", @profile.reload.system_prompt
      patch "/api/v1/sandbox/crew/me", params: { data: { skills: [ { name: "../escape" } ], lock_version: 1 } }, headers: headers, as: :json
      assert_response :unprocessable_entity
      assert_equal "reviewing-code", @profile.reload.skills.first["name"]
    end
  end

  test "a durable session cannot authorize a bot using another bot principal" do
    @db.execute("UPDATE crew_test_sessions SET thread_key = 'slack:T123:A222:C123:999.456' WHERE thread_key LIKE '%A111:%'")
    with_token do |headers|
      get "/api/v1/sandbox/crew/me", headers: headers
      assert_response :forbidden
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

  test "memory is isolated by bot and conversation, with explicit sharing and expiry" do
    @profile.memories.create!(scope_key: "C999", key: "private", content: "Other customer")
    @other.memories.create!(scope_key: "shared", key: "other-bot", content: "Other bot knowledge")
    @profile.memories.create!(scope_key: "C123", key: "expired", content: "Out of date", expires_at: 1.second.ago)
    with_token do |headers|
      put "/api/v1/sandbox/crew/memories", params: { data: { key: "customer", content: "Current customer" } }, headers: headers, as: :json
      assert_response :ok
      assert_equal "slack:T123:A111:C123:123.456", response.parsed_body["data"]["source"]
      put "/api/v1/sandbox/crew/memories", params: { data: { key: "style", content: "Prefer short summaries", scope: "shared" } }, headers: headers, as: :json
      assert_response :ok
      get "/api/v1/sandbox/crew/memories", headers: headers
      assert_response :ok
      assert_equal %w[customer style], response.parsed_body["data"]["memories"].map { |m| m["key"] }.sort
      assert_equal "no-store", response.headers["Cache-Control"]
      get "/api/v1/sandbox/crew/memories?q=SUMMARY", headers: headers
      assert_empty response.parsed_body["data"]["memories"]
      get "/api/v1/sandbox/crew/memories?q=SUMMARIES", headers: headers
      assert_equal [ "style" ], response.parsed_body["data"]["memories"].map { |m| m["key"] }
      # A new thread in the same channel recalls the memory; a new channel cannot.
      @db.execute("UPDATE crew_test_sessions SET thread_key = 'slack:T123:A111:C123:999.001' WHERE thread_key LIKE '%A111:%'")
      get "/api/v1/sandbox/crew/memories", headers: headers
      assert_equal 2, response.parsed_body["data"]["memories"].size
      @db.execute("UPDATE crew_test_sessions SET thread_key = 'slack:T123:A111:D456:999.002' WHERE thread_key LIKE '%A111:%'")
      get "/api/v1/sandbox/crew/memories", headers: headers
      assert_equal [ "style" ], response.parsed_body["data"]["memories"].map { |m| m["key"] }
      delete "/api/v1/sandbox/crew/memories", params: { data: { key: "customer", lock_version: 0 } }, headers: headers, as: :json
      assert_response :not_found
      put "/api/v1/sandbox/crew/memories", params: { data: { key: "private", content: "Overwrite", scope: "C999" } }, headers: headers, as: :json
      assert_response :bad_request
      get "/api/v1/sandbox/crew/memories?app_id=A222", headers: headers
      assert_response :bad_request
    end
    assert_equal "Other customer", @profile.memories.find_by!(scope_key: "C999").content
  end

  test "memory updates and deletes reject stale versions and invalid expiry" do
    with_token do |headers|
      fields = { key: "preference", content: "Original" }
      put "/api/v1/sandbox/crew/memories", params: { data: fields }, headers: headers, as: :json
      assert_response :ok
      put "/api/v1/sandbox/crew/memories", params: { data: fields.merge(content: "New", lock_version: 0) }, headers: headers, as: :json
      assert_response :ok
      put "/api/v1/sandbox/crew/memories", params: { data: fields.merge(lock_version: 0) }, headers: headers, as: :json
      assert_response :conflict
      delete "/api/v1/sandbox/crew/memories", params: { data: { key: "preference", lock_version: 0 } }, headers: headers, as: :json
      assert_response :conflict
      put "/api/v1/sandbox/crew/memories", params: { data: fields.merge(lock_version: 1, expires_at: "not-a-date") }, headers: headers, as: :json
      assert_response :unprocessable_entity
      assert_equal "New", @profile.memories.sole.content
      delete "/api/v1/sandbox/crew/memories", params: { data: { key: "preference", lock_version: 1 } }, headers: headers, as: :json
      assert_response :ok
      put "/api/v1/sandbox/crew/memories", params: { data: fields.merge(lock_version: 1) }, headers: headers, as: :json
      assert_response :conflict
    end
    assert_empty @profile.memories
  end

  test "self improvement records history and restores only owned behavior without approval" do
    @other.update!(system_prompt: "Another bot's new prompt")
    with_token do |headers|
      patch "/api/v1/sandbox/crew/me", params: { data: { system_prompt: "Learned lesson", lock_version: 0 } }, headers: headers, as: :json
      assert_response :ok
      get "/api/v1/sandbox/crew/history?version=0", headers: headers
      assert_response :ok
      assert_equal "", response.parsed_body["data"]["configuration"]["system_prompt"]
      assert_equal "self", response.parsed_body["data"]["revisions"].sole["source"]
      post "/api/v1/sandbox/crew/restore", params: { data: { version: 0, lock_version: 1, app_id: "A222" } }, headers: headers, as: :json
      assert_response :bad_request
      post "/api/v1/sandbox/crew/restore", params: { data: { version: 0, lock_version: 1 } }, headers: headers, as: :json
      assert_response :ok
      assert_equal "", @profile.reload.system_prompt
      assert_equal "Learned lesson", @profile.revisions.find_by!(version: 1).configuration["system_prompt"]
      post "/api/v1/sandbox/crew/restore", params: { data: { version: 1, lock_version: 1 } }, headers: headers, as: :json
      assert_response :conflict
    end
    assert_equal "Another bot's new prompt", @other.reload.system_prompt
    assert_empty @profile.principal.roles
  end

  test "paused apps and released sandboxes cannot read memory or history" do
    client = Object.new
    client.define_singleton_method(:self_get) { |_| raise SlackCrewClient::Error.new("Unavailable", status: 404) }
    Api::V1::Sandbox::CrewController.client_factory = -> { client }
    with_token do |headers|
      get "/api/v1/sandbox/crew/memories", headers: headers
      assert_response :not_found
      get "/api/v1/sandbox/crew/history", headers: headers
      assert_response :not_found
      @db.execute("UPDATE crew_test_sessions SET sandbox_id = NULL")
      get "/api/v1/sandbox/crew/memories", headers: headers
      assert_response :forbidden
    end
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
