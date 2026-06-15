require "test_helper"

class HmacSecretTest < ActiveSupport::TestCase
  def src(role)
    SecretSource.new(source_type: "env", config: { "var" => role.upcase }, role: role, role_kind: "credential_field")
  end

  def build(roles: %w[secret], overrides: {})
    secret = HmacSecret.new({
      namespace: "acme",
      foreign_id: "new-hmac",
      timestamp_format: "unix_seconds",
      signature_algorithm: "sha256",
      signature_key_encoding: "hex",
      signature_output_encoding: "base64",
      signature_message: "{{ .Timestamp }}.{{ .Body }}",
      headers: [ { "name" => "X-Signature", "value" => "{{ .Signature }}" } ],
      created_by: users(:acme_admin)
    }.merge(overrides))
    secret.sources = roles.map { |r| src(r) }
    secret.rules = [ RequestRule.new(host: "hooks.example.com", position: 0) ]
    secret
  end

  test "is valid with the required secret credential, a header and a rule" do
    assert build.valid?
  end

  test "accepts additional named credentials beyond secret" do
    assert build(roles: %w[secret key_id]).valid?
  end

  test "requires the secret credential" do
    secret = build(roles: %w[key_id])
    assert_not secret.valid?
    assert secret.errors[:sources].any? { |m| m.include?("secret") }
  end

  test "rejects an unknown timestamp_format" do
    secret = build(overrides: { timestamp_format: "iso8601" })
    assert_not secret.valid?
    assert secret.errors[:timestamp_format].any?
  end

  test "rejects an unknown signature algorithm" do
    secret = build(overrides: { signature_algorithm: "md5" })
    assert_not secret.valid?
    assert secret.errors[:signature_algorithm].any?
  end

  test "rejects an unknown key_encoding and output_encoding" do
    assert_not build(overrides: { signature_key_encoding: "rot13" }).valid?
    assert_not build(overrides: { signature_output_encoding: "raw" }).valid?
  end

  test "requires a signature_message" do
    secret = build(overrides: { signature_message: nil })
    assert_not secret.valid?
    assert_includes secret.errors[:signature_message], "can't be blank"
  end

  test "requires at least one header with a name and value" do
    assert_not build(overrides: { headers: [] }).valid?
    secret = build(overrides: { headers: [ { "name" => "X-Sig" } ] })
    assert_not secret.valid?
    assert secret.errors[:headers].any?
  end

  test "requires at least one rule" do
    secret = build
    secret.rules = []
    assert_not secret.valid?
    assert_includes secret.errors[:rules], "must include at least one rule"
  end

  test "to_proxy_transform maps credentials, signature, headers and rules" do
    secret = build(roles: %w[secret key_id], overrides: { allow_chunked_body: true })
    secret.save!

    transform = secret.reload.to_proxy_transform
    assert_equal "hmac_sign", transform["name"]

    config = transform["config"]
    assert_equal({ "type" => "env", "var" => "SECRET" }, config.dig("credentials", "secret"))
    assert_equal({ "type" => "env", "var" => "KEY_ID" }, config.dig("credentials", "key_id"))
    assert_equal({ "format" => "unix_seconds" }, config["timestamp"])
    assert_equal "sha256", config.dig("signature", "algorithm")
    assert_equal "hex", config.dig("signature", "key_encoding")
    assert_equal "base64", config.dig("signature", "output_encoding")
    assert_equal "{{ .Timestamp }}.{{ .Body }}", config.dig("signature", "message")
    assert_equal [ { "name" => "X-Signature", "value" => "{{ .Signature }}" } ], config["headers"]
    assert_equal [ { "host" => "hooks.example.com" } ], config["rules"]
    assert_equal true, config["allow_chunked_body"]
  end

  test "to_proxy_transform omits allow_chunked_body when false" do
    secret = build
    secret.save!
    refute secret.reload.to_proxy_transform["config"].key?("allow_chunked_body")
  end

  test "declares hms as its oid prefix" do
    assert_equal "hms", HmacSecret.oid_prefix
  end
end
