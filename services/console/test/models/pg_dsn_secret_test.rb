require "test_helper"

class PgDsnSecretTest < ActiveSupport::TestCase
  def base_attrs(overrides = {})
    {
      namespace: "acme",
      foreign_id: "new-pg",
      database: "new-db",
      created_by: users(:acme_admin)
    }.merge(overrides)
  end

  def with_dsn(secret = nil)
    secret ||= PgDsnSecret.new(base_attrs)
    secret.dsn_source = SecretSource.new(source_type: "env", config: { "var" => "PG_DSN" })
    secret
  end

  def with_inline_dsn(dsn, overrides = {})
    secret = PgDsnSecret.new(base_attrs(overrides))
    secret.dsn_source = SecretSource.new(source_type: "control_plane", secret: dsn)
    secret
  end

  test "is valid with a dsn source" do
    assert with_dsn.valid?
  end

  test "requires a dsn source" do
    secret = PgDsnSecret.new(base_attrs)
    assert_not secret.valid?
    assert_includes secret.errors[:dsn_source], "can't be blank"
  end

  test "requires a foreign_id (it derives the sandbox env var)" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(foreign_id: nil)))
    assert_not secret.valid?
    assert_includes secret.errors[:foreign_id], "can't be blank"
  end

  test "requires a database (it is the routing key)" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(database: nil)))
    assert_not secret.valid?
    assert_includes secret.errors[:database], "can't be blank"
  end

  test "role is optional" do
    assert with_dsn(PgDsnSecret.new(base_attrs(role: nil))).valid?
  end

  test "foreign_id is unique within a namespace" do
    with_dsn(PgDsnSecret.new(base_attrs(foreign_id: "shared-pg", database: "db-a"))).save!
    dup = with_dsn(PgDsnSecret.new(base_attrs(foreign_id: "shared-pg", database: "db-b")))
    assert_not dup.valid?
    assert_includes dup.errors[:foreign_id], "has already been taken"
  end

  test "database is unique within a namespace" do
    with_dsn(PgDsnSecret.new(base_attrs(foreign_id: "first-pg", database: "shared-db"))).save!
    dup = with_dsn(PgDsnSecret.new(base_attrs(foreign_id: "second-pg", database: "shared-db")))
    assert_not dup.valid?
    assert_includes dup.errors[:database], "has already been taken"
  end

  test "an inline DSN whose database matches is valid" do
    assert with_inline_dsn("postgres://u:pw@host:5432/new-db?sslmode=require").valid?
  end

  test "an inline DSN in keyword form whose database matches is valid" do
    assert with_inline_dsn("host=db port=5432 user=u password=pw dbname=new-db").valid?
  end

  test "an inline DSN whose database differs is rejected" do
    secret = with_inline_dsn("postgres://u:pw@host/other-db")
    assert_not secret.valid?
    assert_includes secret.errors[:database], %(must match the DSN database ("other-db"))
  end

  test "an inline DSN naming no database is rejected" do
    secret = with_inline_dsn("host=db user=u password=pw")
    assert_not secret.valid?
    assert_includes secret.errors[:database], %(DSN names no database; it must match "new-db")
  end

  test "a non-inspectable DSN source skips the invariant check" do
    # env source: the value lives on the proxy host, so no mismatch can be raised.
    assert with_dsn(PgDsnSecret.new(base_attrs(database: "anything"))).valid?
  end

  test "to_proxy_dsn keys the entry by foreign_id and carries the dsn, database, and role" do
    secret = pg_dsn_secrets(:acme_analytics_pg)
    entry = secret.to_proxy_dsn

    assert_equal secret.oid, entry["id"]
    assert_equal secret.foreign_id, entry["foreign_id"]
    assert_equal({ "type" => "env", "var" => "PG_ANALYTICS_DSN" }, entry["dsn"])
    assert_equal "analytics", entry["database"]
    assert_equal "readonly", entry["role"]
  end

  test "to_proxy_dsn omits role when blank but always carries database" do
    entry = pg_dsn_secrets(:acme_reporting_pg).to_proxy_dsn

    refute entry.key?("role")
    assert_equal "reporting", entry["database"]
    assert_equal "aws_sm", entry.dig("dsn", "type")
  end

  test "settings default to an empty array and are omitted from to_proxy_dsn" do
    secret = with_dsn
    assert_equal [], secret.settings
    refute secret.to_proxy_dsn.key?("settings")
  end

  test "to_proxy_dsn carries settings as an ordered array of name/value objects" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [
      { "name" => "app.tenant", "value" => "centaur" },
      { "name" => "app.region", "value" => "us" }
    ])))
    assert secret.valid?
    assert_equal(
      [
        { "name" => "app.tenant", "value" => "centaur" },
        { "name" => "app.region", "value" => "us" }
      ],
      secret.to_proxy_dsn["settings"]
    )
  end

  test "to_proxy_dsn resolves value_from principal labels and fields" do
    principal = principals(:acme_channel)
    principal.update!(labels: { "slack_channel_id" => "C0123456789" })
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [
      {
        "name" => "centaur.slack_channel_id",
        "value_from" => { "principal_label" => "slack_channel_id" }
      },
      { "name" => "centaur.principal", "value_from" => { "principal_field" => "foreign_id" } },
      { "name" => "centaur.principal_id", "value_from" => { "principal_field" => "id" } },
      { "name" => "app.tenant", "value" => "centaur" }
    ])))
    assert secret.valid?

    assert_equal(
      [
        { "name" => "centaur.slack_channel_id", "value" => "C0123456789" },
        { "name" => "centaur.principal", "value" => principal.foreign_id },
        { "name" => "centaur.principal_id", "value" => principal.oid },
        { "name" => "app.tenant", "value" => "centaur" }
      ],
      secret.to_proxy_dsn(principal: principal)["settings"]
    )
  end

  test "to_proxy_dsn resolves a label the principal does not carry as an empty string" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [
      { "name" => "centaur.slack_admin", "value_from" => { "principal_label" => "centaur_slack_admin" } }
    ])))

    value = secret.to_proxy_dsn(principal: principals(:acme_channel)).dig("settings", 0, "value")
    assert_equal "", value
  end

  test "to_proxy_dsn resolves value_from as an empty string without a principal" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [
      { "name" => "centaur.slack_channel_id", "value_from" => { "principal_label" => "slack_channel_id" } }
    ])))

    assert_equal "", secret.to_proxy_dsn.dig("settings", 0, "value")
  end

  test "a setting with both value and value_from is rejected" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [
      { "name" => "app.tenant", "value" => "x", "value_from" => { "principal_label" => "team" } }
    ])))
    assert_not secret.valid?
    assert_includes secret.errors[:settings], "[0] value and value_from are mutually exclusive"
  end

  test "a value_from that is not an object is rejected" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [
      { "name" => "app.tenant", "value_from" => "principal_label" }
    ])))
    assert_not secret.valid?
    assert_includes secret.errors[:settings], "[0] value_from must be an object"
  end

  test "a value_from with unknown or extra keys is rejected" do
    [
      { "principal_token" => "x" },
      { "principal_label" => "team", "principal_field" => "name" },
      {}
    ].each do |ref|
      secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [ { "name" => "app.tenant", "value_from" => ref } ])))
      assert_not secret.valid?
      assert_includes secret.errors[:settings],
        "[0] value_from must have exactly one of principal_label or principal_field"
    end
  end

  test "a value_from with a blank principal_label is rejected" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [
      { "name" => "app.tenant", "value_from" => { "principal_label" => "" } }
    ])))
    assert_not secret.valid?
    assert_includes secret.errors[:settings], "[0] principal_label can't be blank"
  end

  test "a value_from with an unknown principal_field is rejected" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [
      { "name" => "app.tenant", "value_from" => { "principal_field" => "labels" } }
    ])))
    assert_not secret.valid?
    assert_includes secret.errors[:settings],
      %([0] unknown principal_field "labels" (one of: id, namespace, foreign_id, name))
  end

  test "settings with a valid empty value are accepted and stringified" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [ { "name" => "app.tenant", "value" => "" } ])))
    assert secret.valid?
    assert_equal "", secret.to_proxy_dsn.dig("settings", 0, "value")
  end

  test "a setting with a blank name is rejected" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [ { "name" => "", "value" => "x" } ])))
    assert_not secret.valid?
    assert_includes secret.errors[:settings], "[0] name is required"
  end

  test "a setting with an invalid GUC name is rejected" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [ { "name" => "bad name!", "value" => "x" } ])))
    assert_not secret.valid?
    assert_includes secret.errors[:settings], %([0] invalid setting name "bad name!")
  end

  test "the reserved role setting name is rejected" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [ { "name" => "ROLE", "value" => "x" } ])))
    assert_not secret.valid?
    assert_includes secret.errors[:settings], %([0] "ROLE" is managed by the proxy; use the role field)
  end

  test "duplicate setting names (case-insensitive) are rejected" do
    secret = with_dsn(PgDsnSecret.new(base_attrs(settings: [
      { "name" => "app.tenant", "value" => "a" },
      { "name" => "App.Tenant", "value" => "b" }
    ])))
    assert_not secret.valid?
    assert_includes secret.errors[:settings], %([1] duplicate setting "App.Tenant")
  end

  test "declares pgs as its oid prefix" do
    assert_equal "pgs", PgDsnSecret.oid_prefix
  end
end
