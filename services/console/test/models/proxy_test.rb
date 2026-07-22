require "test_helper"

class ProxyTest < ActiveSupport::TestCase
  def valid_attrs(overrides = {})
    {
      name: "my-proxy",
      principal: principals(:acme_channel),
      bearer_token_hash: Digest::SHA256.hexdigest("token")
    }.merge(overrides)
  end

  test "is valid with name, principal, and bearer_token_hash" do
    proxy = Proxy.new(valid_attrs(principal: principals(:globex_user)))
    assert proxy.valid?
  end

  test "labels default to an empty hash and accept string labels" do
    proxy = Proxy.create!(
      name: "labels",
      principal: principals(:globex_user),
      labels: { "slack_user_id" => "U123" }
    )

    assert_equal({ "slack_user_id" => "U123" }, proxy.reload.labels)
  end

  test "labels require string values" do
    invalid = Proxy.new(valid_attrs(labels: {
      "centaur.slack_team_id" => 123
    }))

    assert_not invalid.valid?
    assert_includes invalid.errors[:labels], "values must be strings"
  end

  test "requires name" do
    proxy = Proxy.new(valid_attrs(name: nil))
    assert_not proxy.valid?
    assert_includes proxy.errors[:name], "can't be blank"
  end

  test "is valid without a principal (boots unassigned)" do
    proxy = Proxy.new(valid_attrs(principal: nil))
    assert proxy.valid?
    assert_equal "unassigned", proxy.status
    refute proxy.assigned?
  end

  test "stamps principal_assigned_at when a principal is assigned and clears it on unassign" do
    proxy = Proxy.create!(name: "lifecycle", principal: nil)
    assert_nil proxy.principal_assigned_at

    proxy.update!(principal: principals(:globex_user))
    assert proxy.assigned?
    refute_nil proxy.principal_assigned_at

    proxy.update!(principal: nil)
    assert_nil proxy.principal_assigned_at
    assert_equal "unassigned", proxy.status
  end

  test "an unassigned proxy delivers an empty config" do
    proxy = Proxy.create!(name: "idle", principal: nil)
    config = proxy.sync_config_snapshot.fetch(:config)
    assert_empty config["secrets"]
    assert_empty config["transforms"]
    assert_empty config["postgres"]
  end

  test "config_hash changes when the principal is swapped" do
    proxy = Proxy.create!(name: "swap", principal: principals(:globex_user))
    before = proxy.config_hash
    proxy.update!(principal: principals(:acme_channel))
    refute_equal before, proxy.config_hash
  end

  test "config_hash changes when labels change" do
    proxy = Proxy.create!(name: "label-hash", principal: principals(:globex_user))
    before = proxy.config_hash
    proxy.update!(labels: { "centaur.slack_user_id" => "U123" })
    refute_equal before, proxy.config_hash
  end

  test "declares prx as its oid prefix" do
    assert_equal "prx", Proxy.oid_prefix
  end

  test "find_by_oid round-trips" do
    proxy = proxies(:acme_proxy)
    assert_equal proxy, Proxy.find_by_oid(proxy.oid)
  end

  test "issues a plaintext token and matching hash on create" do
    proxy = Proxy.create!(name: "fresh", principal: principals(:globex_user))
    assert proxy.token.start_with?(Proxy::TOKEN_PREFIX)
    assert_match Proxy::TOKEN_FORMAT, proxy.token
    assert_equal Digest::SHA256.hexdigest(proxy.token), proxy.bearer_token_hash
  end

  test "does not overwrite a supplied bearer_token_hash" do
    proxy = Proxy.create!(valid_attrs(principal: principals(:globex_user)))
    assert_nil proxy.token
    assert_equal Digest::SHA256.hexdigest("token"), proxy.bearer_token_hash
  end

  test "find_by_token returns the record for the issued token" do
    proxy = Proxy.create!(name: "lookup", principal: principals(:globex_user))
    assert_equal proxy, Proxy.find_by_token(proxy.token)
  end

  test "find_by_token returns nil for blank or unknown tokens" do
    assert_nil Proxy.find_by_token(nil)
    assert_nil Proxy.find_by_token("")
    assert_nil Proxy.find_by_token("iprx_#{'0' * 64}")
  end

  test "bearer_token_hash is unique" do
    Proxy.create!(name: "first", principal: principals(:globex_user),
                  bearer_token_hash: Digest::SHA256.hexdigest("dup"))
    dup = Proxy.new(name: "second", principal: principals(:globex_user),
                    bearer_token_hash: Digest::SHA256.hexdigest("dup"))
    assert_not dup.valid?
    assert_includes dup.errors[:bearer_token_hash], "has already been taken"
  end

  # --- config_hash --------------------------------------------------------
  # Grant resolution and sync-payload assembly are tested on Principal, which
  # owns that logic; here we cover only how the proxy's hash reacts to changes.

  test "config_hash changes when a pg_dsn grant is added" do
    proxy = Proxy.create!(name: "pg-hashing", principal: principals(:globex_user))
    before = proxy.config_hash
    Grant.create!(principal: proxy.principal, pg_dsn_secret: pg_dsn_secrets(:acme_analytics_pg),
                  created_by: users(:globex_admin))
    refute_equal before, proxy.reload.config_hash
  end

  test "config_hash changes when a transform grant is added" do
    proxy = Proxy.create!(name: "hashing", principal: principals(:globex_user))
    before = proxy.config_hash
    Grant.create!(principal: proxy.principal, gcp_auth_secret: gcp_auth_secrets(:acme_bigquery),
                  created_by: users(:globex_admin))
    refute_equal before, proxy.reload.config_hash
  end

  test "config_hash changes when a role grant becomes reachable" do
    role = Role.create!(namespace: "acme", foreign_id: "extra", created_by: users(:acme_admin))
    proxy = proxies(:acme_proxy)
    before = proxy.config_hash
    Grant.create!(role: role, gcp_auth_secret: gcp_auth_secrets(:acme_bigquery),
                  created_by: users(:acme_admin))
    principals(:acme_channel).principal_roles.create!(role: role)
    refute_equal before, proxy.reload.config_hash
  end
end
