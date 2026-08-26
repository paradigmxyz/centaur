class SlackChannelCatalogSync
  RemoteChannel = Data.define(:id, :name, :private, :archived)
  Identity = Data.define(:team_id, :bot_user_id)

  DEFAULT_API_URL = "https://slack.com/api".freeze
  CHANNEL_TYPES = "public_channel,private_channel".freeze
  OPEN_TIMEOUT_SECONDS = 2
  READ_TIMEOUT_SECONDS = 5
  WRITE_TIMEOUT_SECONDS = 2
  MEMBERSHIP_TTL = 24.hours
  MEMBERSHIP_RETRY_TTL = 1.hour
  MEMBERSHIP_BATCH_SIZE = 100
  REFRESH_LOCK_KEY = "slack_channel_catalog/refreshing".freeze
  REFRESH_LOCK_TTL = 1.minute

  class ChannelNotFoundError < SlackApi::Error; end

  class << self
    def configured?
      token.present?
    end

    def token
      ENV["CENTAUR_CONSOLE_SLACK_BOT_TOKEN"].presence || ENV["SLACK_BOT_TOKEN"].presence
    end

    def api_url
      ENV["SLACK_API_URL"].presence || DEFAULT_API_URL
    end

    def enqueue_if_empty
      return unless configured? && SlackBotChannel.none?
      return unless Rails.cache.write(REFRESH_LOCK_KEY, true, expires_in: REFRESH_LOCK_TTL, unless_exist: true)

      SlackChannelCatalogRefreshJob.perform_later
    rescue StandardError => e
      Rails.cache.delete(REFRESH_LOCK_KEY)
      Rails.logger.warn("Could not enqueue Slack channel catalog sync: #{e.class}: #{e.message}")
    end
  end

  def initialize(token: self.class.token, api_url: self.class.api_url, api: nil)
    raise SlackApi::Error, "SLACK_BOT_TOKEN is not configured." if token.blank?

    @token = token
    @api_url = api_url
    @api = api || HttpClient.new(
      open_timeout: OPEN_TIMEOUT_SECONDS,
      read_timeout: READ_TIMEOUT_SECONDS,
      write_timeout: WRITE_TIMEOUT_SECONDS
    )
  end

  def sync_channels
    identity = fetch_identity
    channels = fetch_channels
    now = Time.current

    SlackBotChannel.transaction do
      SlackBotChannel.update_all(active: false, updated_at: now)
      channels.each { |channel| import_remote_channel(identity, channel, now:) }
    end

    channels.length
  end

  def import_channel(channel_id)
    identity = catalog_identity
    remote = fetch_channel(channel_id)
    import_remote_channel(identity, remote, now: Time.current)
  rescue ChannelNotFoundError
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
    member_ids = fetch_member_user_ids(channel.channel_id)

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

  def fetch_identity
    body = request("auth.test")
    team_id = body["team_id"].to_s
    bot_user_id = body["user_id"].to_s
    raise SlackApi::Error, "Slack auth.test did not return a team ID." if team_id.blank?
    raise SlackApi::Error, "Slack auth.test did not return a bot user ID." if bot_user_id.blank?

    Identity.new(team_id: team_id, bot_user_id: bot_user_id)
  end

  def fetch_channels
    each_page(
      "conversations.list",
      types: CHANNEL_TYPES,
      exclude_archived: "false",
      limit: "200"
    ).filter_map { |channel| parse_channel(channel) }
      .sort_by { |channel| [ channel.name.downcase, channel.id ] }
  end

  def fetch_channel(channel_id)
    body = request("conversations.info", channel: channel_id)
    payload = body["channel"]
    channel = parse_channel(payload)
    raise SlackApi::Error, "Slack conversations.info did not return channel #{channel_id}." unless channel
    channel
  rescue SlackApi::Error => e
    raise unless e.code == "channel_not_found"

    raise ChannelNotFoundError, "The Slack bot cannot access #{channel_id}."
  end

  def fetch_member_user_ids(channel_id)
    each_page("conversations.members", channel: channel_id, limit: "200")
      .map(&:to_s)
      .reject(&:blank?)
      .uniq
      .sort
  end

  def each_page(method, params)
    rows = []
    cursor = nil
    loop do
      body = request(method, **params, cursor: cursor)
      rows.concat(Array(body[method == "conversations.list" ? "channels" : "members"]))
      cursor = body.dig("response_metadata", "next_cursor").to_s
      break if cursor.blank?
    end
    rows
  end

  def request(method, **params)
    response = @api.get(
      "#{@api_url}/#{method}",
      params: params.compact_blank,
      headers: { "Authorization" => "Bearer #{@token}" }
    )
    SlackApi.parse_response!(response, operation: method)
  end

  def parse_channel(channel)
    return unless channel.is_a?(Hash)

    id = channel["id"].to_s
    name = channel["name_normalized"].presence || channel["name"].to_s
    return if id.blank? || name.blank?
    return unless id.start_with?("C", "G")

    RemoteChannel.new(
      id: id,
      name: name,
      private: channel["is_private"] == true,
      archived: channel["is_archived"] == true
    )
  end

  def catalog_identity
    team_id, bot_user_id = SlackBotChannel.limit(1).pick(:team_id, :bot_user_id)
    return Identity.new(team_id: team_id, bot_user_id: bot_user_id) if team_id && bot_user_id

    fetch_identity
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
