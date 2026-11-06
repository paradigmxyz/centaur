# Provisions a Console user's principal. A user first seen in Slack already
# has one, created by api-rs for their Slack DMs and channel turns, so
# when the user's verified Slack SSO identity names it, Console attaches the
# user to that principal rather than creating a second one. Only when no
# principal identifies the user does Console create one, reserving their Slack
# identity on it so a later DM reaches the same principal. A user who ended up
# with both, because they used Console before Console could link their Slack
# identity, has the Console principal merged into the Slack one.
class ConsoleUserPrincipalProvisioner
  USER_MCP_ROLE_FOREIGN_ID = "user-mcp".freeze

  def self.call(user)
    new(user).call
  end

  def initialize(user)
    @user = user
  end

  def call
    Principal.transaction do
      slack_user_id, slack_team_id = UserIdentity.unambiguous_slack_identity(user.user_identities.slack.order(:id))
      slack_foreign_id = Principal.slack_user_foreign_id(slack_team_id, slack_user_id) if slack_user_id
      principal = existing_principal(slack_foreign_id) ||
                  Principal.new(foreign_id: console_foreign_id, created_by: user)
      newly_linked = principal.new_record? || !principal.identified_by?(console_foreign_id)
      # A principal attached from Slack keeps the name api-rs maintains for it.
      principal.name = user.name.presence || user.email if principal.foreign_id == console_foreign_id
      principal.kind = Principal::USER_KIND
      principal.console_user = user
      principal.assign_attributes(slack_user_id: slack_user_id, slack_team_id: slack_team_id) if slack_user_id
      principal.labels = principal.labels.merge("managed-by" => "centaur")
      # Runs once per console execute: skip the write (and its after_commit
      # hooks) when nothing changed.
      principal.save! if principal.changed?
      principal.identities.create!(foreign_id: console_foreign_id) unless principal.identified_by?(console_foreign_id)
      assign_user_mcp_role(principal) if newly_linked
      claim_slack_identity(principal, slack_foreign_id) if slack_foreign_id
      principal
    end
  rescue ActiveRecord::RecordNotUnique
    retry
  end

  private

  attr_reader :user

  def console_foreign_id
    @console_foreign_id ||= Principal.console_user_foreign_id(user)
  end

  def existing_principal(slack_foreign_id)
    by_console = Principal.resolve_foreign_id(console_foreign_id)
    by_slack = slack_foreign_id && Principal.resolve_foreign_id(slack_foreign_id)
    by_slack = nil unless by_slack && attachable?(by_slack)
    return by_console || by_slack unless by_console && by_slack && by_console != by_slack

    # Keep the Slack principal: api-rs binds the user's DM sessions to it and
    # cannot rebind them, while Console's MCP and scheduled-task callers look
    # the principal up afresh through the Console identity the merge carries.
    PrincipalMerge.call(source: by_console, target: by_slack)
    by_slack.reload
  end

  # A user principal that is no one else's: not linked to another Console
  # user. One linked to another user is conflicting evidence and is left alone.
  def attachable?(principal)
    principal.kind == Principal::USER_KIND && principal.console_user_id.in?([ nil, user.id ])
  end

  # Reserves the user's Slack identity for +principal+ before they ever DM.
  # One already held by a principal linked to another user stays there.
  def claim_slack_identity(principal, slack_foreign_id)
    return if Principal.resolve_foreign_id(slack_foreign_id)

    principal.identities.create!(foreign_id: slack_foreign_id)
  end

  def assign_user_mcp_role(principal)
    role = Role
      .create_with(
        name: "User MCP",
        labels: { "managed-by" => "centaur" },
        created_by: user
      )
      .find_or_create_by!(foreign_id: USER_MCP_ROLE_FOREIGN_ID)
    principal.principal_roles.find_or_create_by!(role: role)
  end
end
