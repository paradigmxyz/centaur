require "test_helper"

class GrantTest < ActiveSupport::TestCase
  def valid_attrs(overrides = {})
    {
      principal: principals(:acme_channel),
      static_secret: static_secrets(:github_token_inject),
      created_by: users(:acme_admin)
    }.merge(overrides)
  end

  test "is valid with principal and static_secret" do
    grant = Grant.new(valid_attrs(principal: principals(:globex_user)))
    assert grant.valid?
  end

  test "is valid with a role grantee instead of a principal" do
    grant = Grant.new(valid_attrs(principal: nil, role: roles(:acme_admin_role)))
    assert grant.valid?
  end

  test "requires exactly one grantee" do
    grant = Grant.new(valid_attrs(principal: nil))
    assert_not grant.valid?
    assert_includes grant.errors[:base], "must reference exactly one of principal, role"
  end

  test "rejects both a principal and a role" do
    grant = Grant.new(valid_attrs(role: roles(:acme_infra)))
    assert_not grant.valid?
    assert_includes grant.errors[:base], "must reference exactly one of principal, role"
  end

  test "requires exactly one grantable" do
    grant = Grant.new(valid_attrs(static_secret: nil))
    assert_not grant.valid?
    assert_includes grant.errors[:base], "must reference exactly one of static_secret, gcp_auth_secret, aws_auth_secret, oauth_token_secret, pg_dsn_secret, hmac_secret"
  end

  test "rejects more than one grantable" do
    grant = Grant.new(valid_attrs(gcp_auth_secret: gcp_auth_secrets(:acme_bigquery)))
    assert_not grant.valid?
    assert_includes grant.errors[:base], "must reference exactly one of static_secret, gcp_auth_secret, aws_auth_secret, oauth_token_secret, pg_dsn_secret, hmac_secret"
  end

  test "principal is immutable after creation" do
    grant = grants(:acme_channel_github_token)
    assert_raises(ActiveRecord::ReadonlyAttributeError) do
      grant.update!(principal: principals(:globex_user))
    end
  end

  test "static_secret is immutable after creation" do
    grant = grants(:acme_channel_github_token)
    other = static_secrets(:db_password_replace)
    assert_raises(ActiveRecord::ReadonlyAttributeError) do
      grant.update!(static_secret: other)
    end
  end

  test "destroyed when principal is destroyed" do
    principal = principals(:acme_channel)
    grant_ids = principal.grants.pluck(:id)
    assert_not_empty grant_ids
    principal.destroy!
    assert_equal 0, Grant.where(id: grant_ids).count
  end

  test "destroyed when static_secret is destroyed" do
    ref = static_secrets(:github_token_inject)
    grant_ids = ref.grants.pluck(:id)
    assert_not_empty grant_ids
    ref.destroy!
    assert_equal 0, Grant.where(id: grant_ids).count
  end

  test "destroyed when its role is destroyed" do
    role = roles(:acme_infra)
    grant_ids = role.grants.pluck(:id)
    assert_not_empty grant_ids
    role.destroy!
    assert_equal 0, Grant.where(id: grant_ids).count
  end

  test "grantee returns the principal or role" do
    assert_equal principals(:acme_channel), grants(:acme_channel_github_token).grantee
    assert_equal roles(:acme_infra), grants(:acme_infra_prod_api_key).grantee
  end

  test "declares grant as its oid prefix" do
    assert_equal "grant", Grant.oid_prefix
  end

  test "find_by_oid round-trips" do
    grant = grants(:acme_channel_github_token)
    assert_equal grant, Grant.find_by_oid(grant.oid)
  end

  test "a direct grant defaults to the higher direct priority" do
    grant = Grant.create!(valid_attrs(principal: principals(:globex_user)))
    assert_equal Grant::DEFAULT_DIRECT_PRIORITY, grant.priority
  end

  test "a role grant defaults to the lower role priority" do
    grant = Grant.create!(valid_attrs(principal: nil, role: roles(:globex_infra)))
    assert_equal Grant::DEFAULT_ROLE_PRIORITY, grant.priority
    assert_operator Grant::DEFAULT_DIRECT_PRIORITY, :>, Grant::DEFAULT_ROLE_PRIORITY
  end

  test "an explicit priority overrides the grantee default" do
    grant = Grant.create!(valid_attrs(principal: principals(:globex_user), priority: 5))
    assert_equal 5, grant.priority
  end

  test "priority is mutable after creation" do
    grant = grants(:acme_infra_prod_api_key)
    grant.update!(priority: 250)
    assert_equal 250, grant.reload.priority
  end
end
