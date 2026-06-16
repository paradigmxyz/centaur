require "test_helper"

class ProxySyncControllerTest < ActionDispatch::IntegrationTest
  ACME_TOKEN = "iprx_#{'a' * 64}".freeze

  def auth_headers(token = ACME_TOKEN)
    { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
  end

  def json_body
    JSON.parse(response.body)
  end

  setup do
    @proxy = proxies(:acme_proxy)

    # acme_channel (the proxy's principal) is granted github_token_inject and
    # db_password_replace. Give them sources so they materialize into the sync
    # payload: an env source and an inline control_plane source.
    @inject = static_secrets(:github_token_inject)
    @replace = static_secrets(:db_password_replace)

    SecretSource.create!(source_type: "env", config: { "var" => "GITHUB_TOKEN" }, static_secret: @inject)
    SecretSource.create!(source_type: "control_plane", secret: "s3cr3t-db-pass", static_secret: @replace)

    RequestRule.create!(host: "api.example.com", http_methods: [ "POST" ], paths: [ "/v1/*" ],
                        position: 0, static_secret: @inject)
  end

  test "rejects requests without an Authorization header" do
    post api_v1_proxy_sync_url, params: {}.to_json, headers: { "Content-Type" => "application/json" }
    assert_response :unauthorized
  end

  test "rejects an unknown token" do
    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers("iprx_#{'9' * 64}")
    assert_response :unauthorized
  end

  test "returns config_hash and secrets when no hash is supplied" do
    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok

    body = json_body
    assert_match(/\Asha256:[0-9a-f]{64}\z/, body.fetch("config_hash"))
    secrets = body.fetch("secrets")
    assert_equal 2, secrets.length

    # Omitted top-level fields stay absent so the proxy no-ops on them.
    refute body.key?("rules")
    refute body.key?("mcp")
    refute body.key?("ingest_token")
  end

  test "env source maps inject_config and rules (http_methods -> methods)" do
    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok

    entry = json_body.fetch("secrets").find { |s| s.dig("source", "type") == "env" }
    refute_nil entry
    assert_equal "GITHUB_TOKEN", entry.dig("source", "var")
    assert_equal({ "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" }, entry["inject"])
    assert_nil entry["replace"]

    rule = entry.fetch("rules").first
    assert_equal "api.example.com", rule["host"]
    assert_equal [ "POST" ], rule["methods"]
    assert_equal [ "/v1/*" ], rule["paths"]
    refute rule.key?("http_methods")
  end

  test "control_plane source delivers the decrypted value inline" do
    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok

    entry = json_body.fetch("secrets").find { |s| s.dig("source", "type") == "control_plane" }
    refute_nil entry
    assert_equal "s3cr3t-db-pass", entry.dig("source", "value")
    assert_equal "__DB_PASSWORD__", entry.dig("replace", "proxy_value")
    assert_nil entry["inject"]
  end

  test "matching config_hash returns only the hash, no secrets" do
    current = @proxy.config_hash

    post api_v1_proxy_sync_url, params: { config_hash: current }.to_json, headers: auth_headers
    assert_response :ok

    body = json_body
    assert_equal current, body.fetch("config_hash")
    refute body.key?("secrets")
  end

  test "stale config_hash returns the full payload" do
    post api_v1_proxy_sync_url, params: { config_hash: "sha256:#{'0' * 64}" }.to_json, headers: auth_headers
    assert_response :ok
    assert_equal 2, json_body.fetch("secrets").length
  end

  test "secrets without a source are skipped" do
    # Grant a sourceless static secret to the same principal.
    sourceless = StaticSecret.create!(
      namespace: "acme", name: "no-source",
      inject_config: { "header" => "X-Token" }, created_by: users(:acme_admin)
    )
    Grant.create!(principal: @proxy.principal, static_secret: sourceless, created_by: users(:acme_admin))

    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok
    # Still only the two sourced secrets.
    assert_equal 2, json_body.fetch("secrets").length
  end

  test "config_hash is stable across identical requests" do
    first = @proxy.config_hash
    second = Proxy.find(@proxy.id).config_hash
    assert_equal first, second
  end

  test "transforms carries gcp_auth per grant and a single bundled oauth_token" do
    admin = users(:acme_admin)
    Grant.create!(principal: @proxy.principal, gcp_auth_secret: gcp_auth_secrets(:acme_bigquery), created_by: admin)
    Grant.create!(principal: @proxy.principal, oauth_token_secret: oauth_token_secrets(:acme_gmail_oauth), created_by: admin)

    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok

    transforms = json_body.fetch("transforms")
    names = transforms.map { |t| t["name"] }
    assert_equal 1, names.count("gcp_auth")
    assert_equal 1, names.count("oauth_token")

    oauth = transforms.find { |t| t["name"] == "oauth_token" }
    assert_equal 1, oauth.dig("config", "tokens").length
  end

  test "transforms carries one hmac_sign transform per granted HmacSecret" do
    admin = users(:acme_admin)
    Grant.create!(principal: @proxy.principal, hmac_secret: hmac_secrets(:acme_webhook_hmac), created_by: admin)

    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok

    transforms = json_body.fetch("transforms")
    hmac = transforms.find { |t| t["name"] == "hmac_sign" }
    refute_nil hmac
    assert_equal "sha256", hmac.dig("config", "signature", "algorithm")
    assert_equal({ "type" => "env", "var" => "WEBHOOK_HMAC_KEY" }, hmac.dig("config", "credentials", "secret"))
    assert_equal [ { "host" => "hooks.example.com", "methods" => [ "POST" ] } ], hmac.dig("config", "rules")
  end

  test "transforms is an empty array when no transform grants exist" do
    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok
    assert_equal [], json_body.fetch("transforms")
  end

  test "postgres carries a DSN entry per granted PgDsnSecret, keyed by foreign_id" do
    # acme_channel is granted acme_analytics_pg (see grants.yml).
    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok

    postgres = json_body.fetch("postgres")
    entry = postgres.find { |e| e["foreign_id"] == pg_dsn_secrets(:acme_analytics_pg).foreign_id }
    refute_nil entry
    assert_equal pg_dsn_secrets(:acme_analytics_pg).oid, entry["id"]
    assert_equal "PG_ANALYTICS_DSN", entry.dig("dsn", "var")
    assert_equal "env", entry.dig("dsn", "type")
    assert_equal "readonly", entry["role"]
  end

  test "postgres entries carry pinned session settings when configured" do
    pg = pg_dsn_secrets(:acme_analytics_pg)
    pg.update!(settings: [ { "name" => "app.tenant", "value" => "centaur" } ])

    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok

    entry = json_body.fetch("postgres").find { |e| e["foreign_id"] == pg.foreign_id }
    assert_equal [ { "name" => "app.tenant", "value" => "centaur" } ], entry["settings"]
  end

  test "postgres entries resolve value_from settings against the proxy principal" do
    @proxy.principal.update!(labels: { "slack_channel_id" => "C0123456789" })
    pg = pg_dsn_secrets(:acme_analytics_pg)
    pg.update!(settings: [
      {
        "name" => "centaur.slack_channel_id",
        "value_from" => { "principal_label" => "slack_channel_id" }
      }
    ])

    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok

    entry = json_body.fetch("postgres").find { |e| e["foreign_id"] == pg.foreign_id }
    assert_equal(
      [ { "name" => "centaur.slack_channel_id", "value" => "C0123456789" } ],
      entry["settings"]
    )
  end

  test "directly-granted secrets are emitted after role-granted ones" do
    # acme_channel holds github_token_inject and db_password_replace directly
    # (priority 100) and resolves acme_prod_api_key through the acme_infra role
    # (priority 0). Give the role secret a source so it materializes; lower
    # priority is emitted first, so the direct secrets win iron-proxy's
    # last-transform-wins.
    prod = static_secrets(:acme_prod_api_key)
    SecretSource.create!(source_type: "env", config: { "var" => "PROD_API_KEY" }, static_secret: prod)

    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok

    ids = json_body.fetch("secrets").map { |s| s.dig("source", "var") || s.dig("source", "type") }
    role_index = ids.index("PROD_API_KEY")
    refute_nil role_index
    [ "GITHUB_TOKEN", "control_plane" ].each do |direct|
      assert_operator ids.index(direct), :>, role_index
    end

    # Promote the role grant above the direct grants and it now sorts last.
    grants(:acme_infra_prod_api_key).update!(priority: 500)
    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok
    bumped = json_body.fetch("secrets").map { |s| s.dig("source", "var") || s.dig("source", "type") }
    assert_equal "PROD_API_KEY", bumped.last
  end

  test "an unassigned proxy syncs an empty config with unassigned status" do
    unassigned_token = "iprx_#{'c' * 64}"
    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers(unassigned_token)
    assert_response :ok

    body = json_body
    assert_equal "unassigned", body.fetch("status")
    assert_nil body.fetch("principal_id")
    assert_empty body.fetch("secrets")
    assert_empty body.fetch("transforms")
  end

  test "sync reports the assigned principal and status" do
    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok
    assert_equal "assigned", json_body.fetch("status")
    assert_equal @proxy.principal.oid, json_body.fetch("principal_id")
  end
end
