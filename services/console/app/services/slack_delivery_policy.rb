class SlackDeliveryPolicy
  def initialize(user)
    @user = user
  end

  def allowed?(destination)
    destination = destination.to_s.strip.upcase
    destination == direct_message_user_id || allowed_channels.exists?(channel_id: destination)
  end

  def allowed_channels
    return SlackBotChannel.none unless slack_user_id && slack_team_id

    SlackBotChannel.active
                   .for_team(slack_team_id)
                   .with_members([ slack_user_id ])
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
    @slack_identity ||= UserIdentity.unambiguous_slack_identity(
      user.user_identities.slack.order(:id)
    )
  end
end
