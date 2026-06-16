require "test_helper"

class StaticSecretTest < ActiveSupport::TestCase
  def valid_inject_attrs(overrides = {})
    {
      namespace: "acme",
      foreign_id: "new-ref",
      name: "a friendly name",
      inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
      created_by: users(:acme_admin)
    }.merge(overrides)
  end

  def valid_replace_attrs(overrides = {})
    {
      namespace: "acme",
      foreign_id: "new-ref",
      replace_config: { "proxy_value" => "__TOKEN__" },
      created_by: users(:acme_admin)
    }.merge(overrides)
  end

  test "is valid with inject_config" do
    assert StaticSecret.new(valid_inject_attrs).valid?
  end

  test "is valid with replace_config" do
    assert StaticSecret.new(valid_replace_attrs).valid?
  end

  test "rejects a foreign_id that starts with the opaque id prefix" do
    ref = StaticSecret.new(valid_inject_attrs(foreign_id: "ssr_abc123"))
    assert_not ref.valid?
    assert_includes ref.errors[:foreign_id], "must not start with \"ssr_\", which is reserved for opaque ids"
  end

  test "namespace defaults to 'default' and is valid with no foreign_id or name" do
    ref = StaticSecret.new(
      inject_config: { "header" => "Authorization" },
      created_by: users(:acme_admin)
    )
    assert_equal "default", ref.namespace
    assert ref.valid?, ref.errors.full_messages.inspect
  end

  test "is invalid when namespace is blank" do
    ref = StaticSecret.new(valid_inject_attrs(namespace: ""))
    assert_not ref.valid?
    assert_includes ref.errors[:namespace], "can't be blank"
  end

  test "name is free-form and accepts arbitrary characters" do
    ref = StaticSecret.new(valid_inject_attrs(name: "Anything goes! 1/2, ümlaut."))
    assert ref.valid?, ref.errors.full_messages.inspect
  end

  test "foreign_id is unique within a namespace" do
    existing_attrs = valid_inject_attrs(foreign_id: "shared-fid")
    StaticSecret.create!(existing_attrs)
    dup = StaticSecret.new(existing_attrs.merge(name: "another label"))
    assert_not dup.valid?
    assert_includes dup.errors[:foreign_id], "has already been taken"
  end

  test "same foreign_id is allowed across different namespaces" do
    StaticSecret.create!(valid_inject_attrs(foreign_id: "shared-fid"))
    other = StaticSecret.new(valid_inject_attrs(namespace: "globex", foreign_id: "shared-fid"))
    assert other.valid?
  end

  test "foreign_id rejects non-URL-safe characters" do
    %w[has/slash has\ space].each do |bad|
      ref = StaticSecret.new(valid_inject_attrs(foreign_id: bad))
      assert_not ref.valid?, "expected #{bad.inspect} to be invalid"
      assert ref.errors[:foreign_id].any? { |m| m.include?("URL-safe") }
    end
  end

  test "labels defaults to empty hash" do
    ref = StaticSecret.create!(valid_inject_attrs(foreign_id: "default-labels"))
    assert_equal({}, ref.reload.labels)
  end

  test "must define one of inject_config or replace_config" do
    ref = StaticSecret.new(namespace: "acme", foreign_id: "neither", created_by: users(:acme_admin))
    assert_not ref.valid?
    assert_includes ref.errors[:base], "must define one of inject_config or replace_config"
  end

  test "cannot define both inject_config and replace_config" do
    ref = StaticSecret.new(
      namespace: "acme",
      foreign_id: "both",
      inject_config: { "header" => "Authorization" },
      replace_config: { "proxy_value" => "__TOKEN__" },
      created_by: users(:acme_admin)
    )
    assert_not ref.valid?
    assert_includes ref.errors[:base], "inject_config and replace_config are mutually exclusive"
  end

  test "inject_config requires exactly one of header or query_param" do
    ref = StaticSecret.new(valid_inject_attrs(inject_config: { "formatter" => "x" }))
    assert_not ref.valid?
    assert ref.errors[:inject_config].any?
  end

  test "inject_config with query_param is valid" do
    ref = StaticSecret.new(valid_inject_attrs(inject_config: { "query_param" => "api_key" }))
    assert ref.valid?, ref.errors.full_messages.inspect
  end

  test "inject_config rejects header and query_param together" do
    ref = StaticSecret.new(valid_inject_attrs(
      inject_config: { "header" => "Authorization", "query_param" => "api_key" }
    ))
    assert_not ref.valid?
    assert ref.errors[:inject_config].any?
  end

  test "inject_config rejects unknown keys" do
    ref = StaticSecret.new(valid_inject_attrs(
      inject_config: { "header" => "Authorization", "bogus" => "x" }
    ))
    assert_not ref.valid?
    assert ref.errors[:inject_config].any?
  end

  test "replace_config requires proxy_value" do
    ref = StaticSecret.new(valid_replace_attrs(replace_config: { "match_body" => true }))
    assert_not ref.valid?
    assert ref.errors[:replace_config].any?
  end

  test "replace_config rejects unknown keys" do
    ref = StaticSecret.new(valid_replace_attrs(
      replace_config: { "proxy_value" => "x", "bogus" => true }
    ))
    assert_not ref.valid?
    assert ref.errors[:replace_config].any?
  end

  test "replace_config rejects non-boolean match flags" do
    ref = StaticSecret.new(valid_replace_attrs(
      replace_config: { "proxy_value" => "x", "match_body" => "yes" }
    ))
    assert_not ref.valid?
    assert ref.errors[:replace_config].any?
  end

  test "labels must be a hash" do
    ref = StaticSecret.new(valid_inject_attrs(labels: "nope"))
    assert_not ref.valid?
    assert_includes ref.errors[:labels], "must be a hash"
  end

  test "has_one source association" do
    ref = static_secrets(:github_token_inject)
    src = SecretSource.create!(source_type: "env", config: { "var" => "GITHUB_TOKEN" }, static_secret: ref)
    assert_equal src, ref.reload.source
  end

  test "has_many rules association" do
    ref = static_secrets(:github_token_inject)
    r1 = RequestRule.create!(host: "api.github.com", static_secret: ref)
    r2 = RequestRule.create!(host: "api.example.com", static_secret: ref, position: 1)
    assert_equal [ r1, r2 ], ref.reload.rules.to_a
  end

  test "declares ssr as its oid prefix" do
    assert_equal "ssr", StaticSecret.oid_prefix
  end

  test "find_by_oid round-trips" do
    ref = static_secrets(:github_token_inject)
    assert_equal ref, StaticSecret.find_by_oid(ref.oid)
  end
end
