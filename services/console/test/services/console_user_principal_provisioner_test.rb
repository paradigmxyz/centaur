require "test_helper"

class ConsoleUserPrincipalProvisionerTest < ActiveSupport::TestCase
  SLACK_FOREIGN_ID = "slack-user-t0123456789-u0123456789".freeze

  test "creates and then reuses a scoped principal for the Console user" do
    user = users(:member_user)

    assert_difference -> { Principal.count }, 1 do
      @principal = ConsoleUserPrincipalProvisioner.call(user)
    end
    same_principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_equal @principal, same_principal
    assert_equal "person", @principal.kind
    assert @principal.console_person?
    assert_equal user, @principal.console_user
    assert_equal "centaur", @principal.labels["managed-by"]
    assert_includes @principal.roles, Role.find_by!(foreign_id: "user-mcp")
  end

  test "a user first seen in Slack keeps their Slack person when they sign in with Slack" do
    user = users(:member_user)
    person = slack_person
    person.grants.create!(static_secret: static_secrets(:acme_staging_api_key), created_by: users(:acme_admin))

    assert_no_difference -> { Principal.count } do
      sign_in_with_slack(user)
      assert_equal person, ConsoleUserPrincipalProvisioner.call(user)
    end

    person.reload
    assert person.console_person?
    assert_equal user, person.console_user
    assert_equal "Slack DM @ada", person.name
    assert_equal person, Principal.resolve_foreign_id(Principal.console_user_foreign_id(user))
    assert_includes person.roles, Role.find_by!(foreign_id: "user-mcp")
    assert_includes person.granted_static_secrets, static_secrets(:acme_staging_api_key)
  end

  test "a user who signs in with Slack before ever DMing reserves their Slack identity" do
    user = users(:member_user)
    sign_in_with_slack(user)

    principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_equal principal, Principal.resolve_foreign_id(SLACK_FOREIGN_ID)
  end

  test "a Slack person created before the user linked Slack merges into their Console person" do
    user = users(:member_user)
    principal = ConsoleUserPrincipalProvisioner.call(user)
    person = slack_person
    person.grants.create!(static_secret: static_secrets(:acme_staging_api_key), created_by: users(:acme_admin))

    sign_in_with_slack(user)

    assert_not Principal.exists?(person.id)
    assert_equal principal, Principal.resolve_foreign_id(SLACK_FOREIGN_ID)
    assert_includes principal.granted_static_secrets, static_secrets(:acme_staging_api_key)
  end

  test "a Slack person linked to another Console user is left alone" do
    user = users(:member_user)
    person = slack_person(console_user: users(:acme_admin))
    sign_in_with_slack(user)

    principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_not_equal person, principal
    assert_equal person, Principal.resolve_foreign_id(SLACK_FOREIGN_ID)
    assert_equal users(:acme_admin), person.reload.console_user
  end

  test "repeated calls leave an unchanged principal untouched" do
    user = users(:member_user)
    principal = ConsoleUserPrincipalProvisioner.call(user)

    assert_no_changes -> { principal.reload.updated_at } do
      ConsoleUserPrincipalProvisioner.call(user)
    end
  end

  private

  # The principal api-rs creates for a Slack user's first DM.
  def slack_person(**attributes)
    Principal.create!(foreign_id: SLACK_FOREIGN_ID, name: "Slack DM @ada", kind: "person",
                      slack_user_id: "U0123456789", slack_team_id: "T0123456789",
                      created_by: users(:acme_admin), **attributes)
  end

  def sign_in_with_slack(user)
    user.user_identities.create!(provider: "slack", subject: "U0123456789", team_id: "T0123456789")
  end
end
