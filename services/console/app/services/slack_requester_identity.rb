require "json"
require "net/http"
require "uri"

# Resolves a Console user's GitHub handle through the same authoritative source
# as slackbotv2: the GitHub custom field on the human requester's Slack profile.
class SlackRequesterIdentity
  Result = Data.define(:handle, :source, :reason)
  DEFAULT_API_URL = "https://slack.com/api".freeze
  GITHUB_LABEL = /github/i
  GITHUB_URL = %r{github\.com/([A-Za-z0-9-]{1,39})(?:[/?#]|$)}i
  GITHUB_PREFIX = /github\s*[:=]\s*@?([A-Za-z0-9-]{1,39})/i
  GITHUB_HANDLE = /\A[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?\z/

  def self.resolve(user_ids:)
    token = ENV["CENTAUR_CONSOLE_SLACK_BOT_TOKEN"].presence || ENV["SLACK_BOT_TOKEN"].presence
    return Result.new(handle: nil, source: nil, reason: "Slack bot token is not configured") if token.blank?

    resolver = new(token: token, api_url: ENV["SLACK_API_URL"].presence || DEFAULT_API_URL)
    Array(user_ids).filter_map { |user_id| resolver.resolve(user_id) }.first ||
      Result.new(handle: nil, source: nil, reason: "no GitHub custom field found on Slack profile")
  end

  def initialize(token:, api_url:)
    @token = token
    @api_url = api_url.to_s.delete_suffix("/")
  end

  def resolve(user_id)
    payload = slack_get("users.profile.get", user: user_id, include_labels: "true")
    return nil unless payload["ok"] == true && payload["profile"].is_a?(Hash)

    github_field(payload["profile"])
  rescue StandardError => e
    Rails.logger.warn("console_slack_requester_identity_lookup_failed error=#{e.class}")
    nil
  end

  private

  def slack_get(method, params)
    uri = URI("#{@api_url}/#{method}")
    uri.query = URI.encode_www_form(params)
    request = Net::HTTP::Get.new(uri)
    request["Authorization"] = "Bearer #{@token}"
    request["Accept"] = "application/json"
    response = Net::HTTP.start(uri.host, uri.port, use_ssl: uri.scheme == "https",
                               open_timeout: 2, read_timeout: 5) { |http| http.request(request) }
    JSON.parse(response.body)
  end

  def github_field(profile)
    fields = profile["fields"].is_a?(Hash) ? profile["fields"] : {}
    fields.each_value do |field|
      next unless field.is_a?(Hash)
      label = field["label"].presence || field["alt"].to_s
      value = field["value"].to_s.strip
      next unless label.match?(GITHUB_LABEL) || value.match?(GITHUB_LABEL)

      login = value[GITHUB_URL, 1] || value[GITHUB_PREFIX, 1] || (value if label.match?(GITHUB_LABEL))
      login = login.to_s.delete_prefix("@")
      next unless login.match?(GITHUB_HANDLE)

      return Result.new(handle: "@#{login}", source: "Slack profile custom field \"#{label}\"", reason: nil)
    end
    nil
  end
end
