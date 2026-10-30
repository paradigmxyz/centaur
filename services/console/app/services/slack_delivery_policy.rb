class SlackDeliveryPolicy
  def initialize(user)
    @user = user
  end

  def allowed?(destination)
    destination = destination.to_s.strip.upcase
    destination == direct_message_user_id || allowed_channels.exists?(channel_id: destination)
  end

  def allowed_channels
    channels = SlackBotChannel.active
    channels = channels.for_team(slack_team_id) if slack_team_id
    public_channels = channels.where(private: false)
    return public_channels unless slack_user_id

    private_channels = channels.where(private: true).with_members([ slack_user_id ])
    public_channels.or(private_channels)
  end

  def allowed_channel_ids
    allowed_channels.pluck(:channel_id)
  end

  def direct_message_user_id
    slack_user_id
  end

  def slack_user_id
    slack_identity&.first
  end

  def slack_team_id
    slack_identity&.second
  end

  private

  attr_reader :user

  def slack_identity
    return @slack_identity if defined?(@slack_identity)

    identities = user_identity_candidates + principal_identity_candidates + credential_identity_candidates
    @slack_identity = identities.filter_map { |identity| valid_identity(identity) }.uniq.sole
  rescue Enumerable::SoleItemExpectedError
    @slack_identity = nil
  end

  def user_identity_candidates
    user.user_identities.slack.pluck(:subject, :team_id)
  end

  def principal_identity_candidates
    Principal.where(kind: "console_user", console_user: user).pluck(:slack_user_id, :slack_team_id)
  end

  def credential_identity_candidates
    BrokerCredential.includes(:oauth_app)
                    .joins(:oauth_app)
                    .where(created_by: user, oauth_apps: { provider: Oauth::Providers::Slack::KEY })
                    .map do |credential|
      team_id = credential.labels.to_h["slack_team_id"].presence ||
                credential.oauth_app.labels.to_h["slack_team_id"].presence
      [ credential.provider_subject, team_id ]
    end
  end

  def valid_identity(identity)
    user_id, team_id = identity
    return unless Principal::SLACK_USER_ID_FORMAT.match?(user_id.to_s)
    return unless Principal::SLACK_TEAM_ID_FORMAT.match?(team_id.to_s)

    [ user_id, team_id ]
  end
end
