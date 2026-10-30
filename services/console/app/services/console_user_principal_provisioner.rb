require "digest"

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
      principal = Principal.find_or_initialize_by(foreign_id: foreign_id)
      newly_created = principal.new_record?
      principal.created_by ||= user
      principal.name = user.name.presence || user.email
      principal.kind = "console_user"
      principal.console_user = user
      principal.assign_attributes(slack_identity_fields)
      principal.labels = principal.labels.merge("managed-by" => "centaur")
      # Runs once per console execute: skip the write (and its after_commit
      # hooks) when nothing changed.
      principal.save! if principal.changed?
      assign_user_mcp_role(principal) if newly_created
      principal
    end
  rescue ActiveRecord::RecordNotUnique
    retry
  end

  private

  attr_reader :user

  def foreign_id
    normalized = user.email.to_s.downcase.strip
    safe = normalized.gsub(/[^A-Za-z0-9\-._~]/, "-").gsub(/-+/, "-").first(48)
    digest = Digest::SHA256.hexdigest(normalized).first(12)
    "console-user-#{safe}-#{digest}"
  end

  def slack_identity_fields
    slack_user_id, slack_team_id = UserIdentity.unambiguous_slack_identity(
      user.user_identities.slack.order(:id)
    )
    return {} unless slack_user_id

    { "slack_user_id" => slack_user_id, "slack_team_id" => slack_team_id }
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
