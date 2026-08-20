class SlackChannelCatalogSync
  MEMBERSHIP_TTL = 24.hours
  MEMBERSHIP_RETRY_TTL = 1.hour
  MEMBERSHIP_BATCH_SIZE = 25

  class << self
    def configured?
      token.present?
    end

    def token
      ENV["CENTAUR_CONSOLE_SLACK_BOT_TOKEN"].presence || ENV["SLACK_BOT_TOKEN"].presence
    end

    def api_url
      ENV["SLACK_API_URL"].presence || SlackChannelCatalog::DEFAULT_API_URL
    end
  end

  def initialize(client: nil)
    raise SlackApi::Error, "SLACK_BOT_TOKEN is not configured." unless client || self.class.configured?

    @client = client || SlackChannelCatalog.new(token: self.class.token, api_url: self.class.api_url)
  end

  def sync_channels
    identity = client.fetch_identity
    channels = client.fetch_channels
    now = Time.current

    SlackBotChannel.transaction do
      SlackBotChannel.update_all(active: false, updated_at: now)
      channels.each { |channel| import_remote_channel(identity, channel, now:) }
    end

    channels.length
  end

  def import_channel(channel_id)
    identity = catalog_identity
    remote = client.fetch_channel(channel_id)
    import_remote_channel(identity, remote, now: Time.current)
  rescue SlackChannelCatalog::ChannelNotJoinedError
    SlackBotChannel.where(team_id: identity.team_id, channel_id: channel_id)
                   .update_all(active: false, updated_at: Time.current)
    nil
  end

  def sync_memberships
    channels = stale_memberships
    channels.each do |channel|
      channel_members(channel.channel_id)
    rescue SlackApi::RetryableError
      raise
    rescue StandardError => e
      Rails.logger.warn("Could not sync Slack membership for #{channel.channel_id}: #{e.class}: #{e.message}")
    end
    channels.length
  end

  def channel_members(channel_id)
    channel = SlackBotChannel.find_by!(channel_id: channel_id)
    channel.update!(membership_last_attempted_at: Time.current, membership_error: nil)
    member_ids = client.fetch_member_user_ids(channel.channel_id)
    unless member_ids.include?(channel.bot_user_id)
      raise SlackApi::Error, "Slack membership for #{channel.channel_id} omitted the bot user."
    end

    channel.update!(
      member_user_ids: member_ids,
      membership_refreshed_at: Time.current,
      membership_error: nil
    )
    member_ids
  rescue SlackApi::RetryableError
    channel&.update!(membership_last_attempted_at: nil)
    raise
  rescue StandardError => e
    channel&.update!(membership_last_attempted_at: Time.current, membership_error: e.message)
    raise
  end

  private

  attr_reader :client

  def catalog_identity
    team_id, bot_user_id = SlackBotChannel.limit(1).pick(:team_id, :bot_user_id)
    return SlackChannelCatalog::Identity.new(team_id: team_id, bot_user_id: bot_user_id) if team_id && bot_user_id

    client.fetch_identity
  end

  def import_remote_channel(identity, remote, now:)
    channel = SlackBotChannel.find_or_initialize_by(team_id: identity.team_id, channel_id: remote.id)
    channel.update!(
      bot_user_id: identity.bot_user_id,
      name: remote.name,
      private: remote.private,
      archived: remote.archived,
      active: !remote.archived,
      last_seen_at: now
    )
    channel
  end

  def stale_memberships
    SlackBotChannel.active
                   .where(
                     "membership_refreshed_at IS NULL OR membership_refreshed_at < ?",
                     MEMBERSHIP_TTL.ago
                   )
                   .where(
                     "membership_last_attempted_at IS NULL OR membership_last_attempted_at < ?",
                     MEMBERSHIP_RETRY_TTL.ago
                   )
                   .order(Arel.sql("membership_refreshed_at ASC NULLS FIRST"), :id)
                   .limit(MEMBERSHIP_BATCH_SIZE)
                   .to_a
  end
end
