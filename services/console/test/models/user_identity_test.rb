require "test_helper"

class UserIdentityTest < ActiveSupport::TestCase
  def valid_attrs(overrides = {})
    { user: users(:globex_admin), provider: "google", subject: "sub-123" }.merge(overrides)
  end

  test "a new Slack identity merges the user's Slack DM principal into their Console-user principal" do
    user = users(:member_user)
    principal = ConsoleUserPrincipalProvisioner.call(user)
    dm = Principal.create!(foreign_id: "slack-user-t0123456789-u0123456789", kind: "slack_dm",
                           created_by: users(:acme_admin))

    user.user_identities.create!(provider: "slack", subject: "U0123456789", team_id: "T0123456789")

    assert_not Principal.exists?(dm.id)
    assert_equal principal, Principal.resolve_foreign_id("slack-user-t0123456789-u0123456789")
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
