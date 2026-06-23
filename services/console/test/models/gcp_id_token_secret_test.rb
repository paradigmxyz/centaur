require "test_helper"

class GcpIdTokenSecretTest < ActiveSupport::TestCase
  def base_attrs(overrides = {})
    {
      namespace: "acme",
      foreign_id: "new-cloud-run",
      audience: "https://service-abc123-uc.a.run.app",
      created_by: users(:acme_admin)
    }.merge(overrides)
  end

  def with_keyfile_and_rule(secret = nil)
    secret ||= GcpIdTokenSecret.new(base_attrs)
    secret.keyfile_source = SecretSource.new(source_type: "env", config: { "var" => "GCP_ID_TOKEN_KEYFILE" })
    secret.rules.build(host: "service-abc123-uc.a.run.app", position: 0)
    secret
  end

  test "is valid with a keyfile source audience and rule" do
    assert with_keyfile_and_rule.valid?
  end

  test "requires a keyfile source" do
    secret = GcpIdTokenSecret.new(base_attrs)
    secret.rules.build(host: "service-abc123-uc.a.run.app", position: 0)
    assert_not secret.valid?
    assert_includes secret.errors[:keyfile_source], "can't be blank"
  end

  test "requires an audience" do
    secret = with_keyfile_and_rule(GcpIdTokenSecret.new(base_attrs(audience: "")))
    assert_not secret.valid?
    assert_includes secret.errors[:audience], "can't be blank"
  end

  test "requires at least one rule" do
    secret = GcpIdTokenSecret.new(base_attrs)
    secret.keyfile_source = SecretSource.new(source_type: "env", config: { "var" => "GCP_ID_TOKEN_KEYFILE" })
    assert_not secret.valid?
    assert_includes secret.errors[:rules], "must include at least one rule"
  end

  test "normalizes supported headers" do
    secret = with_keyfile_and_rule(GcpIdTokenSecret.new(base_attrs(header: " X-Serverless-Authorization ")))
    assert secret.valid?, secret.errors.full_messages.inspect
    assert_equal "x-serverless-authorization", secret.header
  end

  test "rejects unsupported headers" do
    secret = with_keyfile_and_rule(GcpIdTokenSecret.new(base_attrs(header: "X-Other")))
    assert_not secret.valid?
    assert_includes secret.errors[:header], "must be one of authorization, x-serverless-authorization"
  end

  test "to_proxy_transform emits gcp_id_token config" do
    config = gcp_id_token_secrets(:acme_cloud_run).to_proxy_transform["config"]
    assert_equal({ "type" => "env", "var" => "CLOUD_RUN_SA_KEYFILE" }, config["keyfile"])
    assert_equal "https://my-service-abc123-uc.a.run.app", config["audience"]
    assert_equal "x-serverless-authorization", config["header"]
    assert_equal [ { "host" => "my-service-abc123-uc.a.run.app" } ], config["rules"]
  end

  test "to_proxy_transform omits the default header" do
    secret = with_keyfile_and_rule
    config = secret.to_proxy_transform["config"]
    refute config.key?("header")
  end

  test "proxy_conflict_targets follows the configured header" do
    assert_equal [ "header:x-serverless-authorization" ],
                 gcp_id_token_secrets(:acme_cloud_run).proxy_conflict_targets
    assert_equal [ "header:authorization" ], with_keyfile_and_rule.proxy_conflict_targets
  end

  test "declares gid as its oid prefix" do
    assert_equal "gid", GcpIdTokenSecret.oid_prefix
  end
end
