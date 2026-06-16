require "test_helper"

class AwsAuthSecretTest < ActiveSupport::TestCase
  def src(role, var)
    SecretSource.new(source_type: "env", config: { "var" => var }, role: role, role_kind: "credential_field")
  end

  def build(roles: { "access_key_id" => "AWS_ACCESS_KEY_ID", "secret_access_key" => "AWS_SECRET_ACCESS_KEY" }, overrides: {})
    secret = AwsAuthSecret.new({
      namespace: "acme",
      foreign_id: "new-aws",
      allowed_services: %w[logs monitoring],
      created_by: users(:acme_admin)
    }.merge(overrides))
    secret.sources = roles.map { |role, var| src(role, var) }
    secret.rules = [ RequestRule.new(host: "logs.us-west-2.amazonaws.com", position: 0) ]
    secret
  end

  test "is valid with access_key_id + secret_access_key, services, and a rule" do
    assert build.valid?
  end

  test "accepts an optional session_token credential" do
    assert build(roles: { "access_key_id" => "AK", "secret_access_key" => "SK", "session_token" => "ST" }).valid?
  end

  test "requires access_key_id and secret_access_key" do
    secret = build(roles: { "access_key_id" => "AK" })
    assert_not secret.valid?
    assert secret.errors[:sources].any? { |m| m.include?("secret_access_key") }
  end

  test "rejects an unknown credential role" do
    secret = build(roles: { "access_key_id" => "AK", "secret_access_key" => "SK", "bogus" => "X" })
    assert_not secret.valid?
    assert secret.errors[:sources].any? { |m| m.include?("unknown") }
  end

  test "rejects non-string allowed_regions and allowed_services" do
    assert_not build(overrides: { allowed_regions: [ 1 ] }).valid?
    assert_not build(overrides: { allowed_services: [ "" ] }).valid?
  end

  test "requires at least one rule" do
    secret = build
    secret.rules = []
    assert_not secret.valid?
    assert secret.errors[:rules].any?
  end

  test "to_proxy_transform emits an aws_auth transform with resolved sources" do
    secret = build(roles: {
      "access_key_id" => "AWS_ACCESS_KEY_ID",
      "secret_access_key" => "AWS_SECRET_ACCESS_KEY",
      "session_token" => "AWS_SESSION_TOKEN"
    })
    secret.save!
    t = secret.to_proxy_transform
    assert_equal "aws_auth", t["name"]
    assert_equal({ "type" => "env", "var" => "AWS_ACCESS_KEY_ID" }, t.dig("config", "access_key_id"))
    assert_equal({ "type" => "env", "var" => "AWS_SECRET_ACCESS_KEY" }, t.dig("config", "secret_access_key"))
    assert_equal({ "type" => "env", "var" => "AWS_SESSION_TOKEN" }, t.dig("config", "session_token"))
    assert_equal %w[logs monitoring], t.dig("config", "allowed_services")
    assert_equal 1, t.dig("config", "rules").length
  end

  test "omits session_token, allowed_regions, and allowed_services when unset" do
    secret = build(overrides: { allowed_regions: [], allowed_services: [] })
    secret.save!
    config = secret.to_proxy_transform["config"]
    assert_not config.key?("session_token")
    assert_not config.key?("allowed_regions")
    assert_not config.key?("allowed_services")
  end

  test "the oid is prefixed aas_" do
    secret = build
    secret.save!
    assert secret.oid.start_with?("aas_")
  end
end
