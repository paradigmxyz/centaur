require "json"

class SlackChannelCatalog
  Channel = Data.define(:id, :name, :private)
  RemoteChannel = Data.define(:id, :name, :private, :archived)
  Identity = Data.define(:team_id, :bot_user_id)
  Result = Data.define(:channels, :error, :configured) do
    def ok?
      error.blank?
    end
  end

  class Error < StandardError; end
  class RetryableApiError < Error
    attr_reader :retry_after

    def initialize(message, retry_after:)
      @retry_after = retry_after
      super(message)
    end
  end

  class ChannelNotJoinedError < Error; end

  DEFAULT_API_URL = "https://slack.com/api".freeze
  DEFAULT_TYPES = "public_channel,private_channel".freeze
  OPEN_TIMEOUT_SECONDS = 2
  READ_TIMEOUT_SECONDS = 5
  WRITE_TIMEOUT_SECONDS = 2
  MAX_RATE_LIMIT_WAIT_SECONDS = 5.minutes.to_i
  TRANSIENT_API_ERRORS = %w[fatal_error internal_error].freeze
  TRANSIENT_RETRY_AFTER_SECONDS = 30

  def initialize(token:, api_url:, api: nil)
    @token = token
    @api_url = api_url.to_s.delete_suffix("/")
    @api = api || HttpClient.new(
      open_timeout: OPEN_TIMEOUT_SECONDS,
      read_timeout: READ_TIMEOUT_SECONDS,
      write_timeout: WRITE_TIMEOUT_SECONDS
    )
  end

  # Compatibility entry point for callers that only need the channel list.
  def fetch
    Result.new(
      channels: fetch_channels.map { |channel| Channel.new(id: channel.id, name: channel.name, private: channel.private) },
      error: nil,
      configured: true
    )
  rescue Error, JSON::ParserError => e
    Result.new(channels: [], error: e.message, configured: true)
  rescue StandardError => e
    Result.new(channels: [], error: "Slack API request failed: #{e.message}", configured: true)
  end

  def fetch_identity
    body = request("auth.test")
    team_id = body["team_id"].to_s
    bot_user_id = body["user_id"].to_s
    raise Error, "Slack auth.test did not return a team ID." if team_id.blank?
    raise Error, "Slack auth.test did not return a bot user ID." if bot_user_id.blank?

    Identity.new(team_id: team_id, bot_user_id: bot_user_id)
  end

  def fetch_channels
    each_page(
      "users.conversations",
      types: DEFAULT_TYPES,
      exclude_archived: "false",
      limit: "200"
    ).filter_map { |channel| parse_channel(channel) }
      .sort_by { |channel| [ channel.name.downcase, channel.id ] }
  end

  def fetch_channel(channel_id)
    body = request("conversations.info", channel: channel_id)
    payload = body["channel"]
    channel = parse_channel(payload)
    raise Error, "Slack conversations.info did not return channel #{channel_id}." unless channel
    if payload.is_a?(Hash) && payload["is_member"] == false
      raise ChannelNotJoinedError, "The Slack bot is not a member of #{channel_id}."
    end

    channel
  end

  def fetch_member_user_ids(channel_id)
    each_page("conversations.members", channel: channel_id, limit: "200")
      .map(&:to_s)
      .reject(&:blank?)
      .uniq
      .sort
  end

  private

  def each_page(method, params)
    rows = []
    cursor = nil
    loop do
      body = request(method, **params, cursor: cursor)
      rows.concat(Array(body[method == "users.conversations" ? "channels" : "members"]))
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
    if response.status == 429
      retry_after = Float(response["retry-after"], exception: false)
      retry_after = 1 unless retry_after&.positive?
      raise RetryableApiError.new(
        "Slack API rate limited #{method}.",
        retry_after: [ retry_after, MAX_RATE_LIMIT_WAIT_SECONDS ].min
      )
    end

    body = response.json
    raise Error, "Slack API returned HTTP #{response.status}." unless response.success?
    if TRANSIENT_API_ERRORS.include?(body["error"])
      raise RetryableApiError.new(
        "Slack API returned #{body['error']} for #{method}.",
        retry_after: TRANSIENT_RETRY_AFTER_SECONDS
      )
    end
    raise Error, "Slack API returned #{body.fetch('error', 'an unknown error')} for #{method}." unless body["ok"] == true

    body
  rescue JSON::ParserError
    raise Error, "Slack API response for #{method} was not JSON."
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
end
