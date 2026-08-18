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
    record = AwsAuthSecret.new
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
      {},
      sources: replacement_sources,
      rules: []
    )
  end

  # A new column must be explicitly classified: either compared (add it to
  # SYNC_CONFIG_REPLACEMENT_ATTRIBUTES) or excluded here. Without this check a
  # forgotten column is invisible to the comparison on both sides, so a PUT
  # changing only that column would be silently skipped as a no-op.
  test "secret source replacement fields cover the schema" do
    owner_foreign_keys = SecretSource::OWNER_ASSOCIATIONS.map do |association|
      SecretSource.reflect_on_association(association).foreign_key.to_s
    end

    assert_equal(
      (SecretSource.attribute_names - %w[id created_at updated_at] - owner_foreign_keys).sort,
      SecretSource::SYNC_CONFIG_REPLACEMENT_ATTRIBUTES.sort
    )
  end

  test "request rule replacement fields cover the schema" do
    owner_foreign_keys = RequestRule::OWNER_ASSOCIATIONS.map do |association|
      RequestRule.reflect_on_association(association).foreign_key.to_s
    end

    assert_equal(
      (RequestRule.attribute_names - %w[id created_at updated_at] - owner_foreign_keys).sort,
      RequestRule::SYNC_CONFIG_REPLACEMENT_ATTRIBUTES.sort
    )
  end
end
