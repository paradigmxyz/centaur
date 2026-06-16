require "test_helper"

class SecretSourceTest < ActiveSupport::TestCase
  def new_source(attrs = {})
    SecretSource.new(attrs)
  end

  test "env source is valid with var" do
    s = new_source(source_type: "env", config: { "var" => "FOO" })
    assert s.valid?
  end

  test "aws_sm source is valid with secret_id" do
    s = new_source(source_type: "aws_sm", config: { "secret_id" => "arn:..." })
    assert s.valid?
  end

  test "aws_ssm source is valid with name" do
    s = new_source(source_type: "aws_ssm", config: { "name" => "/prod/key" })
    assert s.valid?
  end

  test "1password source is valid with secret_ref" do
    s = new_source(source_type: "1password", config: { "secret_ref" => "op://v/i/f" })
    assert s.valid?
  end

  test "1password_connect source is valid with secret_ref" do
    s = new_source(source_type: "1password_connect", config: { "secret_ref" => "op://v/i/f" })
    assert s.valid?
  end

  test "universal json_key is allowed for all source types" do
    SecretSource::SOURCE_TYPES.each do |type|
      required = SecretSource::CONFIG_SCHEMA[type][:required]
      config = required.each_with_object({}) { |k, h| h[k] = "x" }
      # token_broker's credential_id must resolve to a real credential.
      config["credential_id"] = make_broker_credential(access_token: nil).oid if type == "token_broker"
      config["json_key"] = "password"
      config["ttl"] = "5m"
      attrs = { source_type: type, config: config }
      attrs[:secret] = "v" if type == "control_plane"
      s = new_source(attrs)
      assert s.valid?, "expected #{type} with json_key+ttl to be valid, got: #{s.errors.full_messages.inspect}"
    end
  end

  test "control_plane source is valid with a secret" do
    s = new_source(source_type: "control_plane", secret: "supersecret")
    assert s.valid?
  end

  test "control_plane source requires a secret" do
    s = new_source(source_type: "control_plane")
    assert_not s.valid?
    assert s.errors[:secret].any? { |m| m.include?("can't be blank") }
  end

  test "non-control_plane source rejects a secret" do
    s = new_source(source_type: "env", config: { "var" => "FOO" }, secret: "nope")
    assert_not s.valid?
    assert s.errors[:secret].any? { |m| m.include?("only allowed") }
  end

  test "control_plane source secret round-trips through encryption" do
    ref = static_secrets(:github_token_inject)
    s = SecretSource.create!(source_type: "control_plane", secret: "rotated-secret", static_secret: ref)
    assert_equal "rotated-secret", SecretSource.find(s.id).secret
    raw = SecretSource.connection.select_value("SELECT secret FROM secret_sources WHERE id = #{s.id}")
    assert_not_equal "rotated-secret", raw, "expected ciphertext, not plaintext, at rest"
  end

  test "requires source_type" do
    s = new_source(config: { "var" => "FOO" })
    assert_not s.valid?
    assert_includes s.errors[:source_type], "can't be blank"
  end

  test "rejects unknown source_type" do
    s = new_source(source_type: "vault", config: {})
    assert_not s.valid?
    assert_includes s.errors[:source_type], "is not included in the list"
  end

  test "missing required key produces config error per source_type" do
    SecretSource::CONFIG_SCHEMA.each do |type, schema|
      next if schema[:required].empty?
      s = new_source(source_type: type, config: {})
      assert_not s.valid?, "expected #{type} with empty config to be invalid"
      assert s.errors[:config].any? { |m| m.include?("missing required key") },
        "expected missing-key error for #{type}, got: #{s.errors[:config].inspect}"
    end
  end

  test "rejects unknown key in config" do
    s = new_source(source_type: "env", config: { "var" => "FOO", "bogus" => "x" })
    assert_not s.valid?
    assert s.errors[:config].any? { |m| m.include?("unknown key") }
  end

  test "source_type is immutable after creation" do
    s = secret_sources(:env_token)
    assert_raises(ActiveRecord::ReadonlyAttributeError) do
      s.update!(source_type: "aws_sm")
    end
  end

  test "config must be a hash" do
    s = new_source(source_type: "env", config: "not-a-hash")
    assert_not s.valid?
    assert_includes s.errors[:config], "must be a hash"
  end

  test "declares scs as its oid prefix" do
    assert_equal "scs", SecretSource.oid_prefix
  end

  test "find_by_oid round-trips" do
    s = secret_sources(:env_token)
    assert_equal s, SecretSource.find_by_oid(s.oid)
  end

  test "token_broker source is valid referencing a credential by oid" do
    cred = make_broker_credential(access_token: nil)
    s = new_source(source_type: "token_broker", config: { "credential_id" => cred.oid })
    assert s.valid?, s.errors.full_messages.inspect
  end

  test "token_broker source is valid referencing a credential by namespace + foreign_id" do
    cred = make_broker_credential(access_token: nil)
    s = new_source(source_type: "token_broker",
                   config: { "credential_id" => cred.foreign_id, "credential_namespace" => cred.namespace })
    assert s.valid?, s.errors.full_messages.inspect
  end

  test "token_broker source rejects a reference that does not resolve" do
    # A foreign_id in a namespace with no such credential.
    s = new_source(source_type: "token_broker",
                   config: { "credential_id" => "ghost", "credential_namespace" => "nope" })
    assert_not s.valid?
    assert s.errors[:config].any? { |m| m.include?("does not reference an existing broker credential") }

    # A well-formed oid for a credential that no longer exists.
    cred = make_broker_credential(access_token: nil)
    oid = cred.oid
    cred.destroy!
    s2 = new_source(source_type: "token_broker", config: { "credential_id" => oid })
    assert_not s2.valid?
    assert s2.errors[:config].any? { |m| m.include?("does not reference an existing broker credential") }
  end

  test "token_broker foreign_id reference requires a namespace" do
    cred = make_broker_credential(access_token: nil)
    s = new_source(source_type: "token_broker", config: { "credential_id" => cred.foreign_id })
    assert_not s.valid?
    assert s.errors[:config].any? { |m| m.include?("credential_namespace is required") }
  end

  test "token_broker oid reference forbids a namespace" do
    cred = make_broker_credential(access_token: nil)
    s = new_source(source_type: "token_broker",
                   config: { "credential_id" => cred.oid, "credential_namespace" => "default" })
    assert_not s.valid?
    assert s.errors[:config].any? { |m| m.include?("not allowed when credential_id is an opaque id") }
  end

  test "token_broker source rejects unknown config keys" do
    cred = make_broker_credential(access_token: nil)
    s = new_source(source_type: "token_broker", config: { "credential_id" => cred.oid, "failure_ttl" => "30s" })
    assert_not s.valid?
    assert s.errors[:config].any? { |m| m.include?("failure_ttl") }
  end

  test "token_broker source requires credential_id" do
    s = new_source(source_type: "token_broker", config: {})
    assert_not s.valid?
    assert s.errors[:config].any? { |m| m.include?("credential_id") }
  end

  test "token_broker source resolves to a control_plane inline value at sync" do
    cred = make_broker_credential(access_token: "live-token")
    s = new_source(source_type: "token_broker", config: { "credential_id" => cred.oid })
    assert_equal({ "type" => "control_plane", "value" => "live-token" }, s.to_proxy_source)
    assert s.deliverable?
  end

  test "token_broker foreign_id reference resolves at sync" do
    cred = make_broker_credential(access_token: "live-token")
    s = new_source(source_type: "token_broker",
                   config: { "credential_id" => cred.foreign_id, "credential_namespace" => cred.namespace })
    assert_equal({ "type" => "control_plane", "value" => "live-token" }, s.to_proxy_source)
  end

  test "token_broker source is not deliverable when the credential has no token yet" do
    cred = make_broker_credential(access_token: nil) # bootstrapping
    s = new_source(source_type: "token_broker", config: { "credential_id" => cred.oid })
    assert_equal({ "type" => "control_plane", "value" => nil }, s.to_proxy_source)
    assert_not s.deliverable?
  end

  test "token_broker source is not deliverable when the credential is missing" do
    s = new_source(source_type: "token_broker", config: { "credential_id" => "bcr_missing" })
    assert_not s.deliverable?
  end

  # A persisted BrokerCredential. access_token is set via the model so encryption
  # applies.
  def make_broker_credential(access_token:)
    bc = BrokerCredential.create!(namespace: "default", foreign_id: "src-#{SecureRandom.hex(4)}",
                                  token_endpoint: "https://idp.example/token", client_id: "cid",
                                  created_by: users(:acme_admin), refresh_token: "seed")
    bc.update!(access_token: access_token, expires_at: 1.hour.from_now, last_refresh: Time.current) if access_token
    bc
  end

  test "rejects belonging to more than one owner" do
    s = new_source(source_type: "env", config: { "var" => "FOO" },
                   static_secret: static_secrets(:github_token_inject),
                   gcp_auth_secret: gcp_auth_secrets(:acme_bigquery))
    assert_not s.valid?
    assert_includes s.errors[:base], "must belong to at most one of static_secret, gcp_auth_secret, aws_auth_secret, oauth_token_secret, pg_dsn_secret, hmac_secret"
  end

  test "role is only allowed for an oauth_token_secret or hmac_secret source" do
    s = new_source(source_type: "env", config: { "var" => "FOO" },
                   static_secret: static_secrets(:github_token_inject), role: "client_id")
    assert_not s.valid?
    assert_includes s.errors[:role], "is only allowed for a oauth_token_secret or hmac_secret or aws_auth_secret source"
  end

  test "role is required for an oauth_token_secret source" do
    s = new_source(source_type: "env", config: { "var" => "FOO" },
                   oauth_token_secret: oauth_token_secrets(:acme_gmail_oauth))
    assert_not s.valid?
    assert_includes s.errors[:role], "can't be blank for a oauth_token_secret or hmac_secret or aws_auth_secret source"
  end
end
