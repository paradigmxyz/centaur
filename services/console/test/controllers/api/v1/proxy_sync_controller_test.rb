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

  test "sync includes a scoped sandbox entitlements token when signing is configured" do
    with_env(
      "CENTAUR_JWT_SIGNING_SECRET" => "test-secret",
      "CENTAUR_CONSOLE_URL" => "http://centaur-console:3000"
    ) do
      post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    end
    assert_response :ok

    entry = json_body.fetch("secrets").find do |secret|
      secret.dig("inject", "header") == "Authorization" &&
        secret.fetch("rules").any? { |rule| rule["paths"] == [ Proxy::SANDBOX_ENTITLEMENTS_PATH_PATTERN ] }
    end

    refute_nil entry
    assert_equal "Bearer {{ .Value }}", entry.dig("inject", "formatter")
    assert_equal(
      { "host" => "centaur-console", "methods" => [ "GET" ], "paths" => [ Proxy::SANDBOX_ENTITLEMENTS_PATH_PATTERN ] },
      entry.fetch("rules").first
    )

    claims = jwt_payload(entry.dig("source", "value"))
    assert_equal @proxy.name, claims.fetch("sandbox_id")
    assert_equal @proxy.oid, claims.fetch("proxy_id")
    assert_equal @proxy.principal.oid, claims.fetch("principal_id")
    assert_equal "centaur-console-sandbox-entitlements", claims.fetch("aud")
    assert_equal SandboxEntitlements::Jwt::DEFAULT_TTL_SECONDS, claims.fetch("exp") - claims.fetch("iat")
  end

  test "sync omits the sandbox entitlements token when the console URL is not configured" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret", "CENTAUR_CONSOLE_URL" => nil) do
      post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    end
    assert_response :ok

    entry = json_body.fetch("secrets").find do |secret|
      secret.fetch("rules", []).any? { |rule| rule["paths"] == [ Proxy::SANDBOX_ENTITLEMENTS_PATH_PATTERN ] }
    end

    assert_nil entry
  end

  test "cold sync stores an encrypted principal snapshot" do
    assert_difference -> { PrincipalSyncConfigSnapshot.count }, 1 do
      post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    end
    assert_response :ok

    snapshot = PrincipalSyncConfigSnapshot.find_by!(principal: @proxy.principal)
    assert_equal @proxy.principal.sync_config_cache_version, snapshot.principal_cache_version
    assert_equal "s3cr3t-db-pass", snapshot.payload.dig("secrets", 1, "source", "value")

    raw = PrincipalSyncConfigSnapshot.connection.select_value(
      "SELECT payload FROM principal_sync_config_snapshots WHERE id = #{snapshot.id}"
    )
    refute_includes raw, "s3cr3t-db-pass"
  end

  test "secret changes bump principal cache version and build a new snapshot" do
    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok
    original_version = @proxy.principal.reload.sync_config_cache_version

    @replace.source.update!(secret: "rotated-db-pass")

    assert_operator @proxy.principal.reload.sync_config_cache_version, :>, original_version
    assert_difference -> { PrincipalSyncConfigSnapshot.count }, 1 do
      post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    end
    assert_response :ok

    entry = json_body.fetch("secrets").find { |s| s.dig("source", "type") == "control_plane" }
    assert_equal "rotated-db-pass", entry.dig("source", "value")
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

  test "transforms carries gcp auth transforms per grant and a single bundled oauth_token" do
    admin = users(:acme_admin)
    Grant.create!(principal: @proxy.principal, gcp_auth_secret: gcp_auth_secrets(:acme_bigquery), created_by: admin)
    Grant.create!(principal: @proxy.principal, gcp_id_token_secret: gcp_id_token_secrets(:acme_cloud_run),
                  created_by: admin)
    Grant.create!(principal: @proxy.principal, oauth_token_secret: oauth_token_secrets(:acme_gmail_oauth), created_by: admin)

    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok

    transforms = json_body.fetch("transforms")
    names = transforms.map { |t| t["name"] }
    assert_equal 1, names.count("gcp_auth")
    assert_equal 1, names.count("gcp_id_token")
    assert_equal 1, names.count("oauth_token")

    gcp_id = transforms.find { |t| t["name"] == "gcp_id_token" }
    assert_equal "https://my-service-abc123-uc.a.run.app", gcp_id.dig("config", "audience")
    assert_equal "x-serverless-authorization", gcp_id.dig("config", "header")

    oauth = transforms.find { |t| t["name"] == "oauth_token" }
    assert_equal 1, oauth.dig("config", "tokens").length
  end

  test "cached proxy snapshot carries gcp_id_token and invalidates when it changes" do
    admin = users(:acme_admin)
    secret = gcp_id_token_secrets(:acme_cloud_run)
    Grant.create!(principal: @proxy.principal, gcp_id_token_secret: secret, created_by: admin)

    assert_difference -> { PrincipalSyncConfigSnapshot.count }, 1 do
      post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    end
    assert_response :ok

    original_hash = json_body.fetch("config_hash")
    snapshot = PrincipalSyncConfigSnapshot.find_by!(principal: @proxy.principal)
    transform = snapshot.payload.fetch("transforms").find { |t| t["name"] == "gcp_id_token" }
    assert_equal secret.audience, transform.dig("config", "audience")
    assert_equal "CLOUD_RUN_SA_KEYFILE", transform.dig("config", "keyfile", "var")

    assert_no_difference -> { PrincipalSyncConfigSnapshot.count } do
      post api_v1_proxy_sync_url, params: { config_hash: "sha256:#{'0' * 64}" }.to_json,
                                 headers: auth_headers
    end
    assert_response :ok
    transform = json_body.fetch("transforms").find { |t| t["name"] == "gcp_id_token" }
    assert_equal secret.audience, transform.dig("config", "audience")

    original_version = @proxy.principal.reload.sync_config_cache_version
    secret.update!(audience: "https://updated-service-abc123-uc.a.run.app")

    assert_operator @proxy.principal.reload.sync_config_cache_version, :>, original_version
    assert_difference -> { PrincipalSyncConfigSnapshot.count }, 1 do
      post api_v1_proxy_sync_url, params: { config_hash: original_hash }.to_json, headers: auth_headers
    end
    assert_response :ok
    refute_equal original_hash, json_body.fetch("config_hash")
    transform = json_body.fetch("transforms").find { |t| t["name"] == "gcp_id_token" }
    assert_equal "https://updated-service-abc123-uc.a.run.app", transform.dig("config", "audience")
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

  test "postgres carries a DSN entry per granted PgDsnSecret with foreign_id" do
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
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers(unassigned_token)
    end
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

  test "broker credential token changes bump reachable principal cache version" do
    admin = users(:acme_admin)
    credential = BrokerCredential.create!(
      namespace: "acme", foreign_id: "sync-cache-#{SecureRandom.hex(4)}",
      name: "sync cache broker", token_endpoint: "https://oauth.example.com/token",
      client_id: "client", refresh_token: "refresh", access_token: "token-1",
      expires_at: 1.hour.from_now, last_refresh: Time.current, created_by: admin
    )
    secret = StaticSecret.new(
      namespace: "acme", name: "brokered",
      inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
      created_by: admin
    )
    secret.build_source(source_type: "token_broker", config: { "credential_id" => credential.oid })
    secret.rules.build(host: "api.example.com", position: 0)
    secret.save!
    Grant.create!(principal: @proxy.principal, static_secret: secret, created_by: admin)

    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok
    original_version = @proxy.principal.reload.sync_config_cache_version

    credential.update!(access_token: "token-2", expires_at: 2.hours.from_now, last_refresh: Time.current)

    assert_operator @proxy.principal.reload.sync_config_cache_version, :>, original_version
    post api_v1_proxy_sync_url, params: {}.to_json, headers: auth_headers
    assert_response :ok

    entry = json_body.fetch("secrets").find { |s| s.dig("source", "value") == "token-2" }
    refute_nil entry
  end

  def jwt_payload(token)
    _header, payload, _signature = token.split(".")
    JSON.parse(Base64.urlsafe_decode64(payload))
  end

  def with_env(values)
    previous = values.keys.to_h { |key| [ key, ENV[key] ] }
    values.each do |key, value|
      value.nil? ? ENV.delete(key) : ENV[key] = value
    end
    yield
  ensure
    previous.each do |key, value|
      value.nil? ? ENV.delete(key) : ENV[key] = value
    end
  end
end
