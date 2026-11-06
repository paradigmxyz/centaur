require "test_helper"

class ConsoleUserPrincipalProvisionerTest < ActiveSupport::TestCase
  test "creates and then reuses a scoped principal for the Console user" do
    user = users(:member_user)

    assert_difference -> { Principal.count }, 1 do
      @principal = ConsoleUserPrincipalProvisioner.call(user)
    end
    same_principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_equal @principal, same_principal
    assert_equal "console_user", @principal.kind
    assert_equal user, @principal.console_user
    assert_equal "centaur", @principal.labels["managed-by"]
    assert_includes @principal.roles, Role.find_by!(foreign_id: "user-mcp")
  end

  test "a verified Slack identity absorbs the user's existing Slack DM principal" do
    user = users(:member_user)
    user.user_identities.create!(provider: "slack", subject: "U0123456789", team_id: "T0123456789")
    dm = Principal.create!(foreign_id: "slack-user-t0123456789-u0123456789", kind: "slack_dm",
                           slack_user_id: "U0123456789", slack_team_id: "T0123456789",
                           created_by: users(:acme_admin))
    dm.grants.create!(static_secret: static_secrets(:acme_staging_api_key), created_by: users(:acme_admin))

    principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_not Principal.exists?(dm.id)
    assert_equal principal, Principal.resolve_foreign_id("slack-user-t0123456789-u0123456789")
    assert_includes principal.granted_static_secrets, static_secrets(:acme_staging_api_key)
  end

  test "a verified Slack identity reserves the DM foreign_id before the user ever DMs" do
    user = users(:member_user)
    user.user_identities.create!(provider: "slack", subject: "U0123456789", team_id: "T0123456789")

    principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_equal principal, Principal.resolve_foreign_id("slack-user-t0123456789-u0123456789")
  end

  test "a DM principal linked to another Console user is left alone" do
    user = users(:member_user)
    user.user_identities.create!(provider: "slack", subject: "U0123456789", team_id: "T0123456789")
    dm = Principal.create!(foreign_id: "slack-user-t0123456789-u0123456789", kind: "slack_dm",
                           console_user: users(:acme_admin), created_by: users(:acme_admin))

    principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_equal dm, Principal.resolve_foreign_id("slack-user-t0123456789-u0123456789")
    assert_empty principal.foreign_id_aliases
  end

  test "repeated calls leave an unchanged principal untouched" do
    user = users(:member_user)
    principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_no_changes -> { principal.reload.updated_at } do
      ConsoleUserPrincipalProvisioner.call(user)
    end
  end
end
