require "cgi"
require "uri"

class SlackCrewClient
  class Error < StandardError
    attr_reader :status

    def initialize(message = nil, status: nil)
      @status = status
      super(message)
    end
  end

  def initialize(base_url: nil, token: nil, http: nil)
    @base_url = (base_url.presence || ConsoleEnv["SLACK_CREW_URL"].presence).to_s.delete_suffix("/")
    @token = token.presence || ConsoleEnv["SLACK_CREW_ADMIN_TOKEN"].presence
    # Slack manifest operations have a 20-second deadline at the owning service.
    @http = HttpClient.new(http: http, read_timeout: 30)
  end

  def list
    request(:get, "/api/slack/crew")
  end

  def create(attributes)
    request(:post, "/api/slack/crew", attributes)
  end

  def update(id, attributes)
    request(:post, "/api/slack/crew/#{escape(id)}/manage", attributes)
  end

  def self_get(app_id)
    request(:get, "/api/slack/crew/by-app/#{escape(app_id)}/manage")
  end

  def self_update(app_id, attributes)
    request(:post, "/api/slack/crew/by-app/#{escape(app_id)}/manage", attributes)
  end

  private

  def request(method, path, payload = nil)
    raise Error, "Crew service is not configured" if @base_url.blank? || @token.blank?

    response = @http.request(
      method: method,
      url: URI.join("#{@base_url}/", path.delete_prefix("/")).to_s,
      json: payload,
      headers: { "Accept" => "application/json", "Authorization" => "Bearer #{@token}" }
    )
    body = HttpClient.decode_json_body(response.body)
    return body if response.success?

    message = body.is_a?(Hash) ? body["error"] || body["message"] : nil
    raise Error.new(message.presence || "Crew service returned HTTP #{response.status}", status: response.status)
  rescue Error
    raise
  rescue JSON::ParserError
    raise Error.new("Crew service returned an invalid response", status: response&.status)
  rescue StandardError
    raise Error, "Crew service is unavailable"
  end

  def escape(value)
    CGI.escape(value.to_s)
  end
end
