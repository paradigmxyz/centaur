# Provisions a Console user's person principal. A person first seen in Slack
# already has one, created by api-rs for their Slack DMs and channel turns, so
# when the user's verified Slack SSO identity names it, Console attaches the
# user to that principal rather than creating a second one. Only when no
# principal identifies the user does Console create one, reserving their Slack
# identity on it so a later DM reaches the same person.
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
      principal = existing_person(slack_foreign_id) ||
                  Principal.new(foreign_id: console_foreign_id, created_by: user)
      newly_linked = principal.new_record? || !principal.identified_by?(console_foreign_id)
      # A person attached from Slack keeps the name api-rs maintains for it.
      principal.name = user.name.presence || user.email if principal.foreign_id == console_foreign_id
      principal.kind = Principal::PERSON_KIND
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

  def existing_person(slack_foreign_id)
    by_console = Principal.resolve_foreign_id(console_foreign_id)
    return by_console if by_console || slack_foreign_id.nil?

    by_slack = Principal.resolve_foreign_id(slack_foreign_id)
    by_slack if by_slack && attachable?(by_slack)
  end

  # A person who is no one else's: not linked to another Console user. A
  # person linked to another user is conflicting evidence and is left alone.
  def attachable?(principal)
    principal.kind == Principal::PERSON_KIND && principal.console_user_id.in?([ nil, user.id ])
  end

  # Makes the user's Slack identity name +principal+. A separate person
  # already holding it, created from Slack before Console could link the two,
  # is merged in.
  def claim_slack_identity(principal, slack_foreign_id)
    holder = Principal.resolve_foreign_id(slack_foreign_id)
    if holder.nil?
      principal.identities.create!(foreign_id: slack_foreign_id)
    elsif holder != principal && attachable?(holder)
      PrincipalMerge.call(source: holder, target: principal)
    end
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
