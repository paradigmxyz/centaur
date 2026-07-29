require "test_helper"

class SyncConfigReplacementTest < ActiveSupport::TestCase
  def source(role, secret_id:, region:)
    SecretSource.new(
      source_type: "aws_sm",
      config: { "secret_id" => secret_id, "region" => region },
      role: role,
      role_kind: "credential_field"
    )
  end

  def replacement_source(role, secret_id:, region:)
    SecretSource.new(
      source_type: "aws_sm",
      config: { "region" => region, "secret_id" => secret_id },
      role: role,
      role_kind: "credential_field"
    )
  end

  test "multi-source comparison ignores source and config key order" do
    record = AwsAuthSecret.new(namespace: "acme")
    record.sources = [
      source("access_key_id", secret_id: "a", region: "z"),
      source("secret_access_key", secret_id: "z", region: "a")
    ]

    replacement_sources = [
      replacement_source("secret_access_key", secret_id: "z", region: "a"),
      replacement_source("access_key_id", secret_id: "a", region: "z")
    ]

    assert SyncConfigReplacement.equivalent?(
      record,
      { namespace: "acme" },
      { sources: replacement_sources, rules: [] }
    )
  end

  test "comparison rejects missing replacement associations" do
    record = AwsAuthSecret.new(namespace: "acme")

    error = assert_raises(ArgumentError) do
      SyncConfigReplacement.equivalent?(record, { namespace: "acme" }, { sources: [] })
    end

    assert_equal "missing replacement associations: rules", error.message
  end
end
