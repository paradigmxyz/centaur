require "digest"
require "json"
require "net/http"
require "uri"

class SlackChannelCatalog
  Channel = Data.define(:id, :name, :private)
  Result = Data.define(:channels, :error, :configured) do
    def ok?
      error.blank?
    end
  end

  DEFAULT_API_URL = "https://slack.com/api".freeze
  DEFAULT_TYPES = "public_channel,private_channel".freeze
  CACHE_TTL = 5.minutes
  OPEN_TIMEOUT_SECONDS = 2
  READ_TIMEOUT_SECONDS = 5
  WRITE_TIMEOUT_SECONDS = 2

  def self.fetch
    token = ENV["CENTAUR_CONSOLE_SLACK_BOT_TOKEN"].presence || ENV["SLACK_BOT_TOKEN"].presence
    return Result.new(channels: [], error: "SLACK_BOT_TOKEN is not configured.", configured: false) if token.blank?

    api_url = ENV["SLACK_API_URL"].presence || DEFAULT_API_URL
    key = cache_key(token: token, api_url: api_url)
    cached = Rails.cache.read(key)
    return deserialize_result(cached) if cached

    result = new(token: token, api_url: api_url).fetch
    Rails.cache.write(key, serialize_result(result), expires_in: CACHE_TTL) if result.ok?
    result
  end

  def self.cache_key(token:, api_url:)
    token_digest = Digest::SHA256.hexdigest(token)
    api_url_digest = Digest::SHA256.hexdigest(api_url)
    "slack_channel_catalog/v1/#{api_url_digest}/#{token_digest}"
  end

  def self.serialize_result(result)
    {
      "channels" => result.channels.map do |channel|
        { "id" => channel.id, "name" => channel.name, "private" => channel.private }
      end,
      "error" => result.error,
      "configured" => result.configured
    }
  end

  def self.deserialize_result(payload)
    return payload if payload.is_a?(Result)

    channels = Array(payload["channels"]).map do |channel|
      Channel.new(
        id: channel.fetch("id"),
        name: channel.fetch("name"),
        private: channel.fetch("private")
      )
    end
    Result.new(channels: channels, error: payload["error"], configured: payload["configured"])
  end

  def initialize(token:, api_url:)
    @token = token
    @api_url = api_url.to_s.delete_suffix("/")
  end

  def fetch
    channels = []
    cursor = nil
    loop do
      body = request_page(cursor)
      return Result.new(channels: [], error: body.fetch("error", "Slack API request failed."), configured: true) unless body["ok"]

      channels.concat(Array(body["channels"]).filter_map { |channel| parse_channel(channel) })
      cursor = body.dig("response_metadata", "next_cursor").to_s
      break if cursor.blank?
    end

    Result.new(
      channels: channels.sort_by { |channel| [ channel.name.downcase, channel.id ] },
      error: nil,
      configured: true
    )
  rescue JSON::ParserError
    Result.new(channels: [], error: "Slack API response was not JSON.", configured: true)
  rescue StandardError => e
    Result.new(channels: [], error: "Slack API request failed: #{e.message}", configured: true)
  end

  private

  def request_page(cursor)
    uri = URI("#{@api_url}/conversations.list")
    params = {
      types: DEFAULT_TYPES,
      exclude_archived: "true",
      limit: "1000"
    }
    params[:cursor] = cursor if cursor.present?
    uri.query = URI.encode_www_form(params)

    request = Net::HTTP::Get.new(uri)
    request["Authorization"] = "Bearer #{@token}"
    request["Accept"] = "application/json"

    response = Net::HTTP.start(
      uri.host,
      uri.port,
      use_ssl: uri.scheme == "https",
      open_timeout: OPEN_TIMEOUT_SECONDS,
      read_timeout: READ_TIMEOUT_SECONDS,
      write_timeout: WRITE_TIMEOUT_SECONDS
    ) do |http|
      http.request(request)
    end
    return { "ok" => false, "error" => "HTTP #{response.code}" } unless response.code.to_i.between?(200, 299)

    JSON.parse(response.body)
  end

  def parse_channel(channel)
    return nil unless channel.is_a?(Hash)
    id = channel["id"].to_s
    name = channel["name"].to_s
    return nil if id.blank? || name.blank?

    Channel.new(id: id, name: name, private: channel["is_private"] == true)
  end
end
