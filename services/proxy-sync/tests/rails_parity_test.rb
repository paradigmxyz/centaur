require_relative "../../console/test/test_helper"
require "net/http"
require "socket"
require "tempfile"

class RailsParityTest < ActionDispatch::IntegrationTest
  # The Rust process must see committed fixture/model writes.
  self.use_transactional_tests = false
  parallelize(workers: 1)

  # Rails sync leaves snapshot rows behind; drop them so the next fixture
  # load can delete principals created during a test.
  teardown { PrincipalSyncConfigSnapshot.delete_all }

  test "Rust sync matches Rails for populated and hash-only configurations" do
    with_sync_services do
      populate_credentials
      token = "iprx_#{'a' * 64}"
      body = compare(token)
      assert_operator body.fetch("secrets").length, :>=, 102
      assert_equal %w[aws_auth gcp_auth gcp_id_token hmac_sign oauth_token], body.fetch("transforms").map { |t| t.fetch("name") }.sort
      assert_not_empty body.fetch("postgres")
      compare(token, { config_hash: body.fetch("config_hash") })
    end
  end

  test "Rust sync matches Rails after secret rotation and requester assignment" do
    with_sync_services do
      populate_credentials
      token = "iprx_#{'a' * 64}"
      body = compare(token)
      @inline.source.update!(secret: "rotated-parity-value")
      changed = compare(token, { config_hash: body.fetch("config_hash") })
      refute_equal body.fetch("config_hash"), changed.fetch("config_hash")

      bind_requester
      union = compare(token, { config_hash: changed.fetch("config_hash") })
      assert_equal changed.fetch("secrets").length + 1, union.fetch("secrets").length
      compare(token, { config_hash: union.fetch("config_hash") })
    end
  end

  test "Rust sync matches Rails for unassigned proxies and authentication failures" do
    with_sync_services do
      compare("iprx_#{'c' * 64}")
      compare(nil, {}, status: 401)
      compare("iprx_#{'9' * 64}", {}, status: 401)
    end
  end

  # The same scenario runs in proxy-sync's
  # conflicting_credentials_resolve_like_console integration test. Rails
  # agreeing with Rust here confirms that test's expectations match Console.
  test "Rust sync matches Rails for conflicting credentials" do
    with_sync_services do
      token = populate_conflicts
      body = compare(token)
      assert_equal [
        "gcp_auth equal.conflict.test",
        "gcp_auth promoted.conflict.test",
        "oauth_token api.conflict.test X-Api-Token",
        "static *.wild.conflict.test Authorization",
        "static api.conflict.test Authorization",
        "static equal.conflict.test Authorization",
        "static slack.conflict.test Authorization"
      ], served_conflict_credentials(body)
      compare(token, { config_hash: body.fetch("config_hash") })
    end
  end

  private

  def populate_conflicts
    admin = principals(:acme_channel).created_by
    principal = Principal.create!(foreign_id: "conflicts-#{SecureRandom.hex(4)}", kind: "user", created_by: admin)
    role = Role.create!(foreign_id: "conflicts-#{SecureRandom.hex(4)}", created_by: admin)
    principal.principal_roles.create!(role: role)
    grant = lambda do |secret, association, grantee, priority|
      target = grantee == :direct ? { principal: principal } : { role: role }
      Grant.create!(**target, association => secret, created_by: admin, priority: priority)
    end
    static = lambda do |host, inject: nil, replace: nil|
      secret = StaticSecret.new(foreign_id: "conflict-#{SecureRandom.hex(4)}", created_by: admin,
                                inject_config: inject, replace_config: replace)
      secret.build_source(source_type: "env", config: { "var" => "CONFLICT_#{SecureRandom.hex(2).upcase}" })
      secret.rules.build(host: host)
      secret.save!
      secret
    end
    gcp = lambda do |host|
      secret = GcpAuthSecret.new(foreign_id: "conflict-#{SecureRandom.hex(4)}", created_by: admin,
                                 credentials_provider: { "type" => "workload_identity" },
                                 scopes: [ "https://www.googleapis.com/auth/cloud-platform" ])
      secret.rules.build(host: host)
      secret.save!
      secret
    end
    auth = { "header" => "Authorization" }

    # A direct static secret beats a role transform on the same host and
    # header. An OAuth token on a custom header does not conflict with it,
    # but beats a weaker static secret writing that custom header.
    grant.(static.("api.conflict.test", inject: auth), :static_secret, :direct, 100)
    grant.(gcp.("api.conflict.test"), :gcp_auth_secret, :role, 0)
    oauth = OauthTokenSecret.new(foreign_id: "conflict-#{SecureRandom.hex(4)}", name: "custom", grant: "refresh_token",
                                 token_endpoint: "https://oauth2.example/token", scopes: [ "read" ],
                                 header: "X-Api-Token", created_by: admin)
    { "refresh_token" => "CONFLICT_REFRESH", "client_id" => "CONFLICT_CLIENT" }.each do |field, var|
      oauth.sources.build(source_type: "env", config: { "var" => var }, role: field, role_kind: "credential_field")
    end
    oauth.rules.build(host: "api.conflict.test")
    oauth.save!
    grant.(oauth, :oauth_token_secret, :direct, 100)
    grant.(static.("api.conflict.test", inject: { "header" => "X-Api-Token" }), :static_secret, :role, 0)

    # A wildcard host conflicts with a matching exact host.
    grant.(static.("*.wild.conflict.test", inject: auth), :static_secret, :direct, 100)
    grant.(gcp.("bq.wild.conflict.test"), :gcp_auth_secret, :role, 0)

    # A promoted role grant beats a direct grant.
    grant.(static.("promoted.conflict.test", inject: auth), :static_secret, :direct, 100)
    grant.(gcp.("promoted.conflict.test"), :gcp_auth_secret, :role, 900)

    # A replace secret claims its match headers.
    replace = { "proxy_value" => "SLACK_BOT_TOKEN", "match_headers" => [ "Authorization" ] }
    grant.(static.("slack.conflict.test", replace: replace), :static_secret, :role, 0)
    grant.(static.("slack.conflict.test", inject: auth), :static_secret, :direct, 100)

    # Equal priorities are left to the proxy.
    grant.(static.("equal.conflict.test", inject: auth), :static_secret, :direct, 100)
    grant.(gcp.("equal.conflict.test"), :gcp_auth_secret, :direct, 100)

    proxy = Proxy.create!(name: "conflicts-#{SecureRandom.hex(4)}", principal: principal)
    proxy.token
  end

  # Mirrors served_conflict_credentials in proxy-sync's integration tests.
  def served_conflict_credentials(body)
    host = ->(rules) { rules.dig(0, "host").to_s }
    served = body.fetch("secrets").map do |secret|
      header = secret.dig("inject", "header") || secret.dig("replace", "match_headers", 0)
      "static #{host.(secret["rules"])} #{header}"
    end
    body.fetch("transforms").each do |transform|
      if transform["name"] == "oauth_token"
        transform.dig("config", "tokens").each do |token|
          served << "oauth_token #{host.(token["rules"])} #{token["header"] || "Authorization"}"
        end
      else
        served << "#{transform["name"]} #{host.(transform.dig("config", "rules"))}"
      end
    end
    served.select { |line| line.split(" ")[1].end_with?(".conflict.test") }.sort
  end

  def with_sync_services
    binary = File.expand_path(ENV.fetch("PROXY_SYNC_BINARY"))
    assert File.executable?(binary), "Build proxy-sync before running parity tests"
    assert Rails.env.test?

    with_env(
      "CENTAUR_JWT_SIGNING_SECRET" => "parity-test-signing-key",
      "CENTAUR_CONSOLE_URL" => "http://console.example:3000",
      "CENTAUR_API_URL" => "http://api.example:8080"
    ) do
      with_rust(binary) { yield }
    end
  end

  def populate_credentials
    principal = principals(:acme_channel)
    admin = principal.created_by
    proxy = proxies(:acme_proxy)
    proxy.update!(labels: { "sandbox" => "parity" })

    100.times do |i|
      secret = StaticSecret.new(
        foreign_id: "parity-#{i}", created_by: admin,
        inject_config: { "header" => "X-Parity-#{i}", "formatter" => "{{ .Value }}" }
      )
      if i.even?
        secret.build_source(source_type: "control_plane", secret: "parity-value-#{i}")
      else
        secret.build_source(source_type: "env", config: { "var" => "PARITY_#{i}" })
      end
      secret.rules.build(host: "parity-#{i}.example.com", http_methods: %w[GET POST], paths: [ "/v1/*" ])
      secret.save!
      Grant.create!(principal: principal, static_secret: secret, created_by: admin, priority: 100 + i % 3)
      @inline ||= secret
    end

    [
      [ gcp_auth_secrets(:acme_bigquery), :gcp_auth_secret ],
      [ gcp_id_token_secrets(:acme_cloud_run), :gcp_id_token_secret ],
      [ aws_auth_secrets(:acme_cloudwatch_aws), :aws_auth_secret ],
      [ oauth_token_secrets(:acme_gmail_oauth), :oauth_token_secret ],
      [ hmac_secrets(:acme_webhook_hmac), :hmac_secret ]
    ].each_with_index do |(secret, association), i|
      secret.rules.each { |rule| rule.update!(host: "transform-#{i}.example.com") }
      Grant.find_or_create_by!(principal: principal, association => secret) do |grant|
        grant.created_by = admin
        grant.priority = 100
      end
    end
    pg_dsn_secrets(:acme_analytics_pg).update!(settings: [
      { "name" => "app.principal", "value_from" => { "principal_field" => "id" } },
      { "name" => "app.sandbox", "value_from" => { "proxy_label" => "sandbox" } }
    ])
    SlackChannelPermission.create!(principal: principal, channel_id: "C9876543210", upload_enabled: true, download_enabled: true, history_enabled: true)
  end

  def bind_requester
    admin = principals(:acme_channel).created_by
    app = OauthApp.create!(slug: "parity-requester", provider: "github", client_id: "parity-client", client_secret: "parity-secret", allowed_scopes: [ "repo" ], always_available: true, created_by: admin)
    broker = BrokerCredential.create!(
      foreign_id: "parity-requester", token_endpoint: "https://example.com/token",
      client_id: "parity-client", refresh_token: "parity-refresh", access_token: "parity-access",
      expires_at: 1.hour.from_now, last_refresh: Time.current, oauth_app: app, created_by: admin
    )
    wrapper = StaticSecret.new(
      foreign_id: "parity-requester", broker_credential: broker, created_by: admin,
      inject_config: { "header" => "X-Requester", "formatter" => "Bearer {{ .Value }}" }
    )
    wrapper.build_source(source_type: "token_broker", config: { "credential_id" => broker.oid })
    wrapper.rules.build(host: "requester.example.com")
    wrapper.save!
    requester = Principal.create!(foreign_id: "parity-requester", kind: "user", created_by: admin)
    Grant.create!(principal: requester, static_secret: wrapper, created_by: admin)
    proxies(:acme_proxy).update!(requester_principal: requester)
  end

  def with_rust(binary)
    socket = TCPServer.new("127.0.0.1", 0)
    port = socket.addr[1]
    socket.close
    @rust_url = URI("http://127.0.0.1:#{port}/api/v1/proxy/sync")
    encryption = Rails.application.config.active_record.encryption
    Tempfile.create("proxy-sync-parity") do |log|
      pid = Process.spawn({
        "BIND_ADDR" => "127.0.0.1:#{port}",
        "IRON_CONTROL_DATABASE_URL" => ENV.fetch("DATABASE_URL"),
        "IRON_CONTROL_AR_ENCRYPTION_PRIMARY_KEY" => encryption.primary_key,
        "IRON_CONTROL_AR_ENCRYPTION_KEY_DERIVATION_SALT" => encryption.key_derivation_salt
      }, binary, out: log, err: log)
      begin
        ready = false
        100.times do
          begin
            ready = Net::HTTP.get_response(URI("http://127.0.0.1:#{port}/healthz")).code == "200"
          rescue Errno::ECONNREFUSED
            # Wait for database connection and key derivation.
          end
          break if ready
          sleep 0.1
        end
        assert ready, "Rust service did not become ready"
        yield
      ensure
        Process.kill("TERM", pid) rescue Errno::ESRCH
        Process.wait(pid)
      end
    end
  end

  def rails_response(token, payload)
    # Avoid comparing Rust's live reads to Rails' stale-while-revalidate cache.
    PrincipalSyncConfigSnapshot.delete_all
    headers = { "Content-Type" => "application/json" }
    headers["Authorization"] = "Bearer #{token}" if token
    post "/api/v1/proxy/sync", params: payload.to_json, headers: headers
    [ response.status, JSON.parse(response.body) ]
  end

  def compare(token, payload = {}, status: 200)
    3.times do
      before = rails_response(token, payload)
      request = Net::HTTP::Post.new(@rust_url)
      request["Content-Type"] = "application/json"
      request["Authorization"] = "Bearer #{token}" if token
      request.body = payload.to_json
      response = Net::HTTP.start(@rust_url.host, @rust_url.port, open_timeout: 5, read_timeout: 10) { |http| http.request(request) }
      actual = [ response.code.to_i, JSON.parse(response.body) ]
      after = rails_response(token, payload)
      next unless before == after # Retry if a JWT rotation window crossed the requests.

      assert_equal status, actual.first
      # Do not print decrypted credentials or JWTs on failure.
      assert before == actual, "Rails/Rust sync responses differ (payload withheld)"
      return actual.last
    end
    flunk "Could not compare responses within a stable JWT window"
  end
end
