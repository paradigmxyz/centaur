require "test_helper"

class UserIdentityTest < ActiveSupport::TestCase
  def valid_attrs(overrides = {})
    { user: users(:globex_admin), provider: "google", subject: "sub-123" }.merge(overrides)
  end

  test "is valid with user, provider, and subject" do
    assert UserIdentity.new(valid_attrs).valid?
  end

  test "requires a user" do
    identity = UserIdentity.new(valid_attrs(user: nil))
    assert_not identity.valid?
    assert_includes identity.errors[:user], "must exist"
  end

  test "rejects an unsupported provider" do
    identity = UserIdentity.new(valid_attrs(provider: "facebook"))
    assert_not identity.valid?
    assert_includes identity.errors[:provider], "is not included in the list"
  end

  test "subject is unique within a provider" do
    existing = user_identities(:acme_admin_google)
    dup = UserIdentity.new(valid_attrs(provider: existing.provider, subject: existing.subject))
    assert_not dup.valid?
    assert_includes dup.errors[:subject], "has already been taken"
  end

  test "the same subject may exist under a different provider" do
    existing = user_identities(:acme_admin_google)
    other = UserIdentity.new(valid_attrs(provider: "slack", subject: existing.subject))
    assert other.valid?
  end

  test "Feishu identity requires and normalizes tenant and app-scoped open id" do
    identity = UserIdentity.create!(
      valid_attrs(
        provider: "feishu",
        subject: JSON.generate([ "tenant-a", "on-user" ]),
        tenant_key: " tenant-a ",
        open_id: " ou-user "
      )
    )

    assert_equal "tenant-a", identity.tenant_key
    assert_equal "ou-user", identity.open_id
    assert identity.feishu?
  end

  test "non-Feishu identity cannot carry Feishu identity attributes" do
    identity = UserIdentity.new(valid_attrs(tenant_key: "tenant-a", open_id: "ou-user"))

    assert_not identity.valid?
    assert_includes identity.errors[:tenant_key], "is reserved for Feishu identities"
    assert_includes identity.errors[:open_id], "is reserved for Feishu identities"
  end

  test "Feishu delivery identity rejects separators that could change principal parsing" do
    identity = UserIdentity.new(
      valid_attrs(
        provider: "feishu",
        subject: JSON.generate([ "tenant:a", "on-user" ]),
        tenant_key: "tenant:a",
        open_id: "ou-user"
      )
    )

    assert_not identity.valid?
    assert_includes identity.errors[:tenant_key], "contains unsupported characters"
  end

  test "Feishu stable subject must encode the same tenant and a union id" do
    for subject in [
      JSON.generate([ "tenant-b", "on-user" ]),
      JSON.generate([ "tenant-a", "" ]),
      JSON.generate({ tenant_key: "tenant-a", union_id: "on-user" })
    ]
      identity = UserIdentity.new(
        valid_attrs(provider: "feishu", subject:, tenant_key: "tenant-a", open_id: "ou-user")
      )

      assert_not identity.valid?
      assert_includes identity.errors[:subject], "must encode the Feishu tenant and union ID"
    end
  end

  test "email is normalized to lowercase and stripped" do
    identity = UserIdentity.create!(valid_attrs(email: "  Mixed@Case.EXAMPLE  "))
    assert_equal "mixed@case.example", identity.email
  end

  test "declares usid as its oid prefix" do
    assert_equal "usid", UserIdentity.oid_prefix
  end

  test "find_by_oid round-trips" do
    identity = user_identities(:acme_admin_google)
    assert_equal identity, UserIdentity.find_by_oid(identity.oid)
  end
end
