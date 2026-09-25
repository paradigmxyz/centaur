require_relative "../../console/test/test_helper"
require "net/http"
require "socket"
require "tempfile"

class RailsParityTest < ActionDispatch::IntegrationTest
  # The Rust process must see committed fixture/model writes.
  self.use_transactional_tests = false
  parallelize(workers: 1)

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

  private

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
