require "test_helper"

class ProxyTest < ActiveSupport::TestCase
  include ActiveSupport::Testing::TimeHelpers

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

  test "is valid without a requester principal" do
    proxy = Proxy.new(valid_attrs)
    assert proxy.valid?
    assert_nil proxy.requester_principal_id
  end

  test "stamps requester_principal_assigned_at on assign, updates it on swap, and clears it on unassign" do
    proxy = Proxy.create!(name: "requester-lifecycle", principal: nil)
    assert_nil proxy.requester_principal_assigned_at

    proxy.update!(requester_principal: principals(:acme_user_alice))
    first_stamp = proxy.requester_principal_assigned_at
    refute_nil first_stamp

    travel 1.minute do
      proxy.update!(requester_principal: principals(:acme_user_bob))
      refute_equal first_stamp, proxy.requester_principal_assigned_at
    end

    proxy.update!(requester_principal: nil)
    assert_nil proxy.requester_principal_assigned_at
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

  # --- requester principal ------------------------------------------------

  def build_requester
    Principal.create!(foreign_id: "requester-#{SecureRandom.hex(4)}",
                      kind: "user", created_by: users(:acme_admin))
  end

  test "destroying the requester principal unbinds it and keeps the proxy" do
    requester = build_requester
    proxy = Proxy.create!(name: "requester-fk", principal: principals(:acme_channel),
                          requester_principal: requester)

    requester.destroy!

    proxy.reload
    assert_nil proxy.requester_principal_id
    assert_equal principals(:acme_channel), proxy.principal
  end
end
