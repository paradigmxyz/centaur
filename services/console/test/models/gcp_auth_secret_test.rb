require "test_helper"

class GcpAuthSecretTest < ActiveSupport::TestCase
  def base_attrs(overrides = {})
    {
      namespace: "acme",
      foreign_id: "new-gcp",
      scopes: [ "https://www.googleapis.com/auth/cloud-platform" ],
      created_by: users(:acme_admin)
    }.merge(overrides)
  end

  def with_keyfile(secret = nil)
    secret ||= GcpAuthSecret.new(base_attrs)
    secret.keyfile_source = SecretSource.new(source_type: "env", config: { "var" => "GCP_SA" })
    secret
  end

  test "is valid with a keyfile source" do
    assert with_keyfile.valid?
  end

  test "is valid with credentials_provider" do
    secret = GcpAuthSecret.new(base_attrs(credentials_provider: { "type" => "workload_identity" }))
    assert secret.valid?, secret.errors.full_messages.inspect
  end

  test "requires exactly one credential mode" do
    secret = GcpAuthSecret.new(base_attrs)
    assert_not secret.valid?
    assert_includes secret.errors[:base], "must define one of keyfile (source) or credentials_provider"
  end

  test "rejects both keyfile and credentials_provider" do
    secret = with_keyfile(GcpAuthSecret.new(base_attrs(credentials_provider: { "type" => "workload_identity" })))
    assert_not secret.valid?
    assert_includes secret.errors[:base], "keyfile and credentials_provider are mutually exclusive"
  end

  test "requires at least one scope" do
    secret = with_keyfile(GcpAuthSecret.new(base_attrs(scopes: [])))
    assert_not secret.valid?
    assert_includes secret.errors[:scopes], "must include at least one scope"
  end

  test "scopes must be strings" do
    secret = with_keyfile(GcpAuthSecret.new(base_attrs(scopes: [ 1 ])))
    assert_not secret.valid?
    assert_includes secret.errors[:scopes], "must all be strings"
  end

  test "subject is rejected with credentials_provider" do
    secret = GcpAuthSecret.new(base_attrs(
      credentials_provider: { "type" => "workload_identity" },
      subject: "user@acme.example"
    ))
    assert_not secret.valid?
    assert_includes secret.errors[:subject], "is only allowed with a keyfile source"
  end

  test "subject is allowed with a keyfile source" do
    secret = with_keyfile(GcpAuthSecret.new(base_attrs(subject: "user@acme.example")))
    assert secret.valid?, secret.errors.full_messages.inspect
  end

  test "credentials_provider must be a known type" do
    secret = GcpAuthSecret.new(base_attrs(credentials_provider: { "type" => "bogus" }))
    assert_not secret.valid?
    assert secret.errors[:credentials_provider].any?
  end

  test "credentials_provider rejects unknown keys" do
    secret = GcpAuthSecret.new(base_attrs(credentials_provider: { "type" => "workload_identity", "x" => 1 }))
    assert_not secret.valid?
    assert secret.errors[:credentials_provider].any?
  end

  test "foreign_id is unique within a namespace" do
    with_keyfile(GcpAuthSecret.new(base_attrs(foreign_id: "shared-gcp"))).save!
    dup = with_keyfile(GcpAuthSecret.new(base_attrs(foreign_id: "shared-gcp")))
    assert_not dup.valid?
    assert_includes dup.errors[:foreign_id], "has already been taken"
  end

  test "to_proxy_transform with credentials_provider" do
    secret = gcp_auth_secrets(:acme_bigquery)
    transform = secret.to_proxy_transform
    assert_equal "gcp_auth", transform["name"]
    config = transform["config"]
    assert_equal({ "type" => "workload_identity" }, config["credentials_provider"])
    assert_equal [ "https://www.googleapis.com/auth/cloud-platform" ], config["scopes"]
    assert_equal [ { "host" => "*.googleapis.com" } ], config["rules"]
    refute config.key?("keyfile")
    refute config.key?("subject")
  end

  test "to_proxy_transform with keyfile source and subject" do
    secret = gcp_auth_secrets(:acme_gcs_keyfile)
    config = secret.to_proxy_transform["config"]
    assert_equal({ "type" => "env", "var" => "GCP_SA_KEYFILE" }, config["keyfile"])
    assert_equal "storage-bot@acme.example", config["subject"]
    refute config.key?("credentials_provider")
  end

  test "declares gas as its oid prefix" do
    assert_equal "gas", GcpAuthSecret.oid_prefix
  end
end
