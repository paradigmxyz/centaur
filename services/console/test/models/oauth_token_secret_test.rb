require "test_helper"

class OauthTokenSecretTest < ActiveSupport::TestCase
  def src(role, kind: "credential_field")
    SecretSource.new(source_type: "env", config: { "var" => role.upcase }, role: role, role_kind: kind)
  end

  def build(grant:, roles:, overrides: {})
    secret = OauthTokenSecret.new({
      namespace: "acme",
      foreign_id: "new-oauth",
      grant: grant,
      token_endpoint: "https://login.example.com/token",
      created_by: users(:acme_admin)
    }.merge(overrides))
    secret.sources = roles.map { |r| src(r) }
    secret.rules = [ RequestRule.new(host: "api.example.com", position: 0) ]
    secret
  end

  test "refresh_token grant is valid with refresh_token + client_id" do
    assert build(grant: "refresh_token", roles: %w[refresh_token client_id]).valid?
  end

  test "refresh_token grant accepts an optional client_secret" do
    assert build(grant: "refresh_token", roles: %w[refresh_token client_id client_secret]).valid?
  end

  test "refresh_token grant requires client_id" do
    secret = build(grant: "refresh_token", roles: %w[refresh_token])
    assert_not secret.valid?
    assert secret.errors[:sources].any? { |m| m.include?("client_id") }
  end

  test "client_credentials grant requires client_id and client_secret" do
    assert build(grant: "client_credentials", roles: %w[client_id client_secret]).valid?
    secret = build(grant: "client_credentials", roles: %w[client_id])
    assert_not secret.valid?
    assert secret.errors[:sources].any? { |m| m.include?("client_secret") }
  end

  test "password grant requires username, password, client_id" do
    assert build(grant: "password", roles: %w[username password client_id]).valid?
    secret = build(grant: "password", roles: %w[username client_id])
    assert_not secret.valid?
    assert secret.errors[:sources].any? { |m| m.include?("password") }
  end

  test "jwt_bearer grant requires issuer, subject, private_key and audience" do
    assert build(grant: "jwt_bearer", roles: %w[issuer subject private_key],
                 overrides: { audience: "account.docusign.com" }).valid?
  end

  test "jwt_bearer grant requires audience" do
    secret = build(grant: "jwt_bearer", roles: %w[issuer subject private_key])
    assert_not secret.valid?
    assert_includes secret.errors[:audience], "can't be blank for the jwt_bearer grant"
  end

  test "rejects fields not used by the grant" do
    secret = build(grant: "client_credentials", roles: %w[client_id client_secret refresh_token])
    assert_not secret.valid?
    assert secret.errors[:sources].any? { |m| m.include?("not used by the client_credentials grant") }
  end

  test "rejects an unknown grant" do
    secret = build(grant: "device_code", roles: %w[client_id])
    assert_not secret.valid?
    assert secret.errors[:grant].any?
  end

  test "requires a token_endpoint" do
    secret = build(grant: "refresh_token", roles: %w[refresh_token client_id], overrides: { token_endpoint: nil })
    assert_not secret.valid?
    assert_includes secret.errors[:token_endpoint], "can't be blank"
  end

  test "requires at least one rule" do
    secret = build(grant: "refresh_token", roles: %w[refresh_token client_id])
    secret.rules = []
    assert_not secret.valid?
    assert_includes secret.errors[:rules], "must include at least one rule"
  end

  test "to_proxy_entry maps fields, headers, scopes and rules" do
    secret = build(
      grant: "refresh_token",
      roles: %w[refresh_token client_id],
      overrides: { scopes: [ "gmail.readonly" ], header: "X-Auth", value_prefix: "Token " }
    )
    secret.sources += [ src("x-api-key", kind: "endpoint_header") ]
    secret.save!

    entry = secret.reload.to_proxy_entry
    assert_equal "refresh_token", entry["grant"]
    assert_equal "https://login.example.com/token", entry["token_endpoint"]
    assert_equal({ "type" => "env", "var" => "REFRESH_TOKEN" }, entry["refresh_token"])
    assert_equal({ "type" => "env", "var" => "CLIENT_ID" }, entry["client_id"])
    assert_equal [ "gmail.readonly" ], entry["scopes"]
    assert_equal "X-Auth", entry["header"]
    assert_equal "Token ", entry["value_prefix"]
    assert_equal({ "x-api-key" => { "type" => "env", "var" => "X-API-KEY" } }, entry["token_endpoint_headers"])
    assert_equal [ { "host" => "api.example.com" } ], entry["rules"]
  end

  test "to_proxy_entry omits optional keys when unset" do
    secret = build(grant: "client_credentials", roles: %w[client_id client_secret])
    secret.save!
    entry = secret.reload.to_proxy_entry
    refute entry.key?("audience")
    refute entry.key?("scopes")
    refute entry.key?("header")
    refute entry.key?("token_endpoint_headers")
  end

  test "declares ots as its oid prefix" do
    assert_equal "ots", OauthTokenSecret.oid_prefix
  end
end
