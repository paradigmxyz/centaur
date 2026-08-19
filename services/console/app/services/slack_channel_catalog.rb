require "json"

class SlackChannelCatalog
  Channel = Data.define(:id, :name, :private)
  Result = Data.define(:channels, :error, :configured) do
    def ok?
      error.blank?
    end
  end

  DEFAULT_API_URL = "https://slack.com/api".freeze
  DEFAULT_TYPES = "public_channel,private_channel".freeze
  OPEN_TIMEOUT_SECONDS = 2
  READ_TIMEOUT_SECONDS = 5
  WRITE_TIMEOUT_SECONDS = 2

  def initialize(token:, api_url:, api: nil)
    @token = token
    @api_url = api_url.to_s.delete_suffix("/")
    @api = api || HttpClient.new(
      open_timeout: OPEN_TIMEOUT_SECONDS,
      read_timeout: READ_TIMEOUT_SECONDS,
      write_timeout: WRITE_TIMEOUT_SECONDS
    )
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
    params = {
      types: DEFAULT_TYPES,
      exclude_archived: "true",
      limit: "1000"
    }
    params[:cursor] = cursor if cursor.present?

    response = @api.get(
      "#{@api_url}/conversations.list",
      params: params,
      headers: { "Authorization" => "Bearer #{@token}" }
    )
    return { "ok" => false, "error" => "HTTP #{response.status}" } unless response.success?

    response.json
  end

  def parse_channel(channel)
    return nil unless channel.is_a?(Hash)
    id = channel["id"].to_s
    name = channel["name"].to_s
    return nil if id.blank? || name.blank?

    Channel.new(id: id, name: name, private: channel["is_private"] == true)
  end
end
