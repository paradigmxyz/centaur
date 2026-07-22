require "test_helper"

class ProxiesControllerTest < ActionDispatch::IntegrationTest
  ACME_TOKEN = "iak_acme-ci-token".freeze
  ACME_PROXY_TOKEN = "iprx_#{'a' * 64}".freeze

  def auth_headers(token = ACME_TOKEN)
    { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
  end

  def json_body
    JSON.parse(response.body)
  end

  test "rejects requests without an Authorization header" do
    get api_v1_proxies_url
    assert_response :unauthorized
  end

  test "GET index lists proxies" do
    get api_v1_proxies_url, headers: auth_headers
    assert_response :ok
    ids = json_body.fetch("data").map { |d| d["id"] }
    assert_includes ids, proxies(:acme_proxy).oid
    assert_includes ids, proxies(:globex_proxy).oid
  end

  test "GET index filters by principal_id" do
    get api_v1_proxies_url, params: { principal_id: principals(:acme_channel).oid }, headers: auth_headers
    assert_response :ok
    ids = json_body.fetch("data").map { |d| d["id"] }
    assert_includes ids, proxies(:acme_proxy).oid
    refute_includes ids, proxies(:globex_proxy).oid
  end

  test "GET index filters by labels" do
    proxies(:acme_proxy).update!(labels: { "centaur.slack_user_id" => "U123" })
    get api_v1_proxies_url,
        params: { labels: { "centaur.slack_user_id" => "U123" } },
        headers: auth_headers
    assert_response :ok

    ids = json_body.fetch("data").map { |d| d["id"] }
    assert_includes ids, proxies(:acme_proxy).oid
    refute_includes ids, proxies(:globex_proxy).oid
  end

  test "GET show returns a proxy" do
    proxy = proxies(:acme_proxy)
    get api_v1_proxy_url(id: proxy.oid), headers: auth_headers
    assert_response :ok
    data = json_body.fetch("data")
    assert_equal proxy.oid, data["id"]
    assert_equal proxy.principal.oid, data["principal_id"]
    assert_equal proxy.labels, data["labels"]
    refute data.key?("token")
  end

  test "POST creates a proxy and returns the plaintext token once" do
    body = {
      data: {
        name: "edge-proxy",
        principal_id: principals(:acme_channel).oid,
        labels: { "centaur.slack_user_id" => "U123" }
      }
    }
    assert_difference -> { Proxy.count }, 1 do
      post api_v1_proxies_url, params: body.to_json, headers: auth_headers
    end
    assert_response :created

    data = json_body.fetch("data")
    token = data.fetch("token")
    assert_match Proxy::TOKEN_FORMAT, token
    assert_equal Proxy.find_by_token(token).oid, data["id"]
    assert_equal({ "centaur.slack_user_id" => "U123" }, data["labels"])
  end

  test "POST returns the sync config hash including sandbox entitlements" do
    body = {
      data: {
        name: "edge-proxy-with-entitlements",
        principal_id: principals(:acme_channel).oid
      }
    }

    with_env(
      "CENTAUR_JWT_SIGNING_SECRET" => "test-secret",
      "CENTAUR_CONSOLE_URL" => "http://centaur-console:3000"
    ) do
      post api_v1_proxies_url, params: body.to_json, headers: auth_headers
      assert_response :created
      data = json_body.fetch("data")

      post api_v1_proxy_sync_url, params: {}.to_json, headers: proxy_auth_headers(data.fetch("token"))
      assert_response :ok
      assert_equal data.fetch("config_hash"), json_body.fetch("config_hash")
    end
  end

  test "POST with invalid labels returns a validation error" do
    body = { data: { name: "bad-labels", labels: { "slack_user_id" => 123 } } }
    post api_v1_proxies_url, params: body.to_json, headers: auth_headers
    assert_response :unprocessable_entity
    assert_equal "validation failed", json_body.dig("error", "message")
    assert_includes json_body.dig("error", "details", "labels"), "values must be strings"
  end

  test "POST with a missing name returns a validation error" do
    body = { data: { principal_id: principals(:acme_channel).oid } }
    post api_v1_proxies_url, params: body.to_json, headers: auth_headers
    assert_response :unprocessable_entity
    assert_equal "validation failed", json_body.dig("error", "message")
  end

  test "POST with an unknown principal returns not found" do
    body = { data: { name: "x", principal_id: "prn_doesnotexist" } }
    post api_v1_proxies_url, params: body.to_json, headers: auth_headers
    assert_response :not_found
  end

  test "POST without a principal creates an unassigned proxy" do
    body = { data: { name: "boots-unassigned" } }
    post api_v1_proxies_url, params: body.to_json, headers: auth_headers
    assert_response :created

    data = json_body.fetch("data")
    assert_nil data["principal_id"]
    assert_equal "unassigned", data["status"]
    assert_nil data["principal_assigned_at"]
    assert_match Proxy::TOKEN_FORMAT, data.fetch("token")
  end

  test "PATCH assigns a principal to an unassigned proxy" do
    proxy = proxies(:unassigned_proxy)
    body = { data: { principal_id: principals(:acme_channel).oid } }
    patch api_v1_proxy_url(id: proxy.oid), params: body.to_json, headers: auth_headers
    assert_response :ok

    data = json_body.fetch("data")
    assert_equal principals(:acme_channel).oid, data["principal_id"]
    assert_equal "assigned", data["status"]
    refute_nil data["principal_assigned_at"]
  end

  test "PATCH swaps the principal of an assigned proxy" do
    proxy = proxies(:acme_proxy)
    body = { data: { principal_id: principals(:globex_user).oid } }
    patch api_v1_proxy_url(id: proxy.oid), params: body.to_json, headers: auth_headers
    assert_response :ok
    assert_equal principals(:globex_user).oid, json_body.dig("data", "principal_id")
    assert_equal principals(:globex_user), proxy.reload.principal
  end

  test "PATCH omitting labels leaves labels unchanged" do
    proxy = proxies(:acme_proxy)
    proxy.update!(labels: { "centaur.slack_user_id" => "U1" })
    body = { data: { principal_id: principals(:globex_user).oid } }

    patch api_v1_proxy_url(id: proxy.oid), params: body.to_json, headers: auth_headers
    assert_response :ok
    assert_equal({ "centaur.slack_user_id" => "U1" }, json_body.dig("data", "labels"))
    assert_equal({ "centaur.slack_user_id" => "U1" }, proxy.reload.labels)
  end

  test "PATCH replaces labels" do
    proxy = proxies(:acme_proxy)
    proxy.update!(labels: { "centaur.slack_user_id" => "U1" })
    body = { data: { labels: { "centaur.slack_user_id" => "U2" } } }

    patch api_v1_proxy_url(id: proxy.oid), params: body.to_json, headers: auth_headers
    assert_response :ok
    assert_equal({ "centaur.slack_user_id" => "U2" }, json_body.dig("data", "labels"))
    assert_equal({ "centaur.slack_user_id" => "U2" }, proxy.reload.labels)
  end

  test "PATCH returns the sync config hash including sandbox entitlements" do
    proxy = proxies(:acme_proxy)
    body = { data: { labels: { "centaur.slack_user_id" => "U2" } } }

    with_env(
      "CENTAUR_JWT_SIGNING_SECRET" => "test-secret",
      "CENTAUR_CONSOLE_URL" => "http://centaur-console:3000"
    ) do
      patch api_v1_proxy_url(id: proxy.oid), params: body.to_json, headers: auth_headers
      assert_response :ok
      mutation_hash = json_body.dig("data", "config_hash")

      post api_v1_proxy_sync_url, params: {}.to_json, headers: proxy_auth_headers(ACME_PROXY_TOKEN)
      assert_response :ok
      assert_equal mutation_hash, json_body.fetch("config_hash")
    end
  end

  test "PATCH with null labels clears labels" do
    proxy = proxies(:acme_proxy)
    proxy.update!(labels: { "centaur.slack_user_id" => "U1" })
    body = { data: { labels: nil } }

    patch api_v1_proxy_url(id: proxy.oid), params: body.to_json, headers: auth_headers
    assert_response :ok
    assert_equal({}, json_body.dig("data", "labels"))
    assert_equal({}, proxy.reload.labels)
  end

  test "PATCH with non-hash labels returns a validation error" do
    proxy = proxies(:acme_proxy)
    proxy.update!(labels: { "centaur.slack_user_id" => "U1" })
    body = { data: { labels: "U2" } }

    patch api_v1_proxy_url(id: proxy.oid), params: body.to_json, headers: auth_headers
    assert_response :unprocessable_entity
    assert_equal "validation failed", json_body.dig("error", "message")
    assert_includes json_body.dig("error", "details", "labels"), "must be a hash"
    assert_equal({ "centaur.slack_user_id" => "U1" }, proxy.reload.labels)
  end

  test "PATCH with a null principal_id unassigns the proxy" do
    proxy = proxies(:acme_proxy)
    body = { data: { principal_id: nil } }
    patch api_v1_proxy_url(id: proxy.oid), params: body.to_json, headers: auth_headers
    assert_response :ok

    data = json_body.fetch("data")
    assert_nil data["principal_id"]
    assert_equal "unassigned", data["status"]
    assert_nil proxy.reload.principal
  end

  test "PATCH with an unknown principal returns not found" do
    proxy = proxies(:unassigned_proxy)
    body = { data: { principal_id: "prn_doesnotexist" } }
    patch api_v1_proxy_url(id: proxy.oid), params: body.to_json, headers: auth_headers
    assert_response :not_found
  end

  test "GET show reports an unassigned proxy" do
    proxy = proxies(:unassigned_proxy)
    get api_v1_proxy_url(id: proxy.oid), headers: auth_headers
    assert_response :ok
    data = json_body.fetch("data")
    assert_nil data["principal_id"]
    assert_equal "unassigned", data["status"]
  end

  test "DELETE removes a proxy" do
    proxy = proxies(:globex_proxy)
    assert_difference -> { Proxy.count }, -1 do
      delete api_v1_proxy_url(id: proxy.oid), headers: auth_headers
    end
    assert_response :no_content
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

  def proxy_auth_headers(token)
    { "Authorization" => "Bearer #{token}", "Content-Type" => "application/json" }
  end
end
