require "test_helper"

class PersonalSshKeyGrantsTest < ActiveSupport::TestCase
  SLACK_USER = "U0SSHOWNER1".freeze
  SLACK_TEAM = "T0SSHTEAM01".freeze

  setup do
    @user = users(:member_user)
  end

  def save_key(user = @user)
    user.create_personal_ssh_key!(name: "My SSH key", created_by: user, private_key: synthetic_ssh_key.first)
  end

  def principal(kind:, **attributes)
    Principal.create!(foreign_id: "ssh-#{SecureRandom.hex(4)}", kind: kind, created_by: users(:acme_admin), **attributes)
  end

  def link_slack(user = @user, subject: SLACK_USER, team_id: SLACK_TEAM)
    user.user_identities.create!(provider: "slack", subject: subject, team_id: team_id, email: user.email)
  end

  def granted?(principal, key)
    principal.grants.exists?(ssh_key_secret: key)
  end

  test "saving a key grants it to the owner's console and verified Slack DM principals only" do
    console = principal(kind: "console_user", console_user: @user)
    link_slack
    dm = principal(kind: "slack_dm", slack_user_id: SLACK_USER, slack_team_id: SLACK_TEAM)
    other_workspace = principal(kind: "slack_dm", slack_user_id: SLACK_USER, slack_team_id: "T0OTHERTEAM")
    channel = principal(kind: "slack_channel", slack_user_id: SLACK_USER, slack_team_id: SLACK_TEAM)
    someone_else = principal(kind: "console_user", console_user: users(:acme_admin))

    key = save_key

    assert granted?(console, key)
    assert granted?(dm, key)
    assert_not granted?(other_workspace, key)
    assert_not granted?(channel, key)
    assert_not granted?(someone_else, key)
  end

  test "principals and Slack identities that appear later receive the key" do
    key = save_key
    console = principal(kind: "console_user", console_user: @user)
    assert granted?(console, key)

    dm = principal(kind: "slack_dm", slack_user_id: SLACK_USER, slack_team_id: SLACK_TEAM)
    assert_not granted?(dm, key), "no verified Slack identity yet"
    link_slack
    assert granted?(dm, key)
  end

  test "an ambiguous Slack account, inactive owner, or email match never grants" do
    link_slack
    link_slack(subject: "U0SSHOWNER2", team_id: SLACK_TEAM)
    key = save_key
    dm = principal(kind: "slack_dm", slack_user_id: SLACK_USER, slack_team_id: SLACK_TEAM)
    assert_not granted?(dm, key)

    same_email = principal(kind: "user", slack_email: @user.email)
    assert_not granted?(same_email, key)

    @user.update!(status: :disabled)
    console = principal(kind: "console_user", console_user: @user)
    assert_not granted?(console, key)
  end

  test "granting is idempotent" do
    console = principal(kind: "console_user", console_user: @user)
    key = save_key
    assert_equal 0, PersonalSshKeyGrants.new.apply_for_user(@user)
    assert_equal 1, console.grants.where(ssh_key_secret: key).count
  end
end
