# Creates a Slack channel or Slack user principal before its first session,
# with the identity api-rs derives when a session starts there
# (crates/centaur-iron-control/src/principal.rs). Later sessions resolve to
# this principal instead of creating another one, which is what lets an admin
# preapprove a conversation.
class SlackPrincipalProvisioner
  InvalidRequest = Class.new(StandardError)

  class Conflict < StandardError
    attr_reader :principal

    def initialize(principal)
      @principal = principal
      super("principal #{principal.foreign_id} already exists")
    end
  end

  KINDS = %w[slack_channel slack_dm].freeze
  CHANNEL_ID_FORMAT = /\A[CG][A-Z0-9]{8,}\z/

  # slack_id is a channel ID (C… or G…) for slack_channel and a user ID for
  # slack_dm. A user principal is always scoped to slack_team_id, the
  # workspace the user is known to belong to. display_name is the channel or
  # user name without its # or @ prefix.
  def initialize(kind:, slack_id:, slack_team_id:, display_name:, created_by:)
    @kind = kind
    @slack_id = slack_id.to_s.strip.upcase
    @slack_team_id = slack_team_id
    @display_name = display_name.to_s.strip.presence
    @created_by = created_by
  end

  def call
    principal = build
    existing = Principal.find_by(foreign_id: principal.foreign_id) ||
      (principal.slack_channel_id && Principal.find_by(slack_channel_id: principal.slack_channel_id))
    raise Conflict, existing if existing

    Principal.transaction do
      principal.apply_default_sandbox_capabilities!
      principal.save!
      # api-rs writes a new channel principal's permission for its own channel
      # only when it creates the principal, so write it here instead.
      if principal.slack_channel_id
        principal.slack_channel_permissions.create!(
          channel_id: principal.slack_channel_id, **SlackChannelPermission::DEFAULT_ENABLED_ATTRIBUTES
        )
      end
    end
    principal
  rescue ActiveRecord::RecordNotUnique
    raise Conflict, Principal.find_by!(foreign_id: principal.foreign_id)
  end

  private

  attr_reader :kind, :slack_id, :slack_team_id, :display_name, :created_by

  def build
    case kind
    when "slack_channel" then channel_principal
    when "slack_dm" then user_principal
    else raise InvalidRequest, "kind must be one of #{KINDS.join(", ")}"
    end
  end

  # Channel principals are not scoped by team, so a Slack Connect channel
  # keeps one principal whichever workspace a message comes from.
  def channel_principal
    unless slack_id.match?(CHANNEL_ID_FORMAT)
      raise InvalidRequest, "slack_channel_id must be a Slack channel ID (C… or G…); DMs use kind slack_dm"
    end

    new_principal(
      foreign_id: "slack-channel-#{slack_id.downcase}",
      name: display_name ? "Slack Channel ##{display_name}" : "Slack Channel #{slack_id}",
      slack_channel_id: slack_id
    )
  end

  def user_principal
    unless slack_id.match?(Principal::SLACK_USER_ID_FORMAT)
      raise InvalidRequest, "slack_user_id must be a Slack user ID (U… or W…)"
    end

    new_principal(
      foreign_id: "slack-user-#{slack_team_id.downcase}-#{slack_id.downcase}",
      name: display_name ? "Slack DM @#{display_name}" : "Slack User #{slack_id} (team #{slack_team_id})",
      slack_user_id: slack_id,
      slack_team_id: slack_team_id
    )
  end

  def new_principal(**attributes)
    Principal.new(kind: kind, labels: { "managed-by" => "centaur" }, created_by: created_by, **attributes)
  end
end
