module Mercator
  class ConnectionClient
    ORIGIN = Oauth::Providers::Mercator::ORIGIN

    def initialize(http: HttpClient.new(max_body_bytes: 128 * 1024))
      @http = http
    end

    def register(redirect_uri)
      response = @http.post("#{ORIGIN}/oauth/register", json: {
        client_name: "Centaur", redirect_uris: [ redirect_uri ],
        grant_types: %w[authorization_code refresh_token], response_types: [ "code" ],
        token_endpoint_auth_method: "client_secret_post", scope: Oauth::Providers::Mercator::SCOPE
      })
      body = checked_json(response)
      unless body["client_id"].is_a?(String) && body["client_id"].present? &&
             body["client_secret"].is_a?(String) && body["client_secret"].present?
        raise Broker::ExchangeError.new("Mercator client registration failed", stage: "parse")
      end
      body.slice("client_id", "client_secret")
    rescue Broker::ExchangeError
      raise
    rescue StandardError
      raise Broker::ExchangeError.new("Mercator registration unavailable", stage: "network")
    end

    def status(access_token)
      response = @http.post("#{ORIGIN}/mcp/auth",
        headers: { "Authorization" => "Bearer #{access_token}", "Accept" => "application/json, text/event-stream" },
        json: { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "get_connection_status", arguments: {} } })
      body = checked_json(response)
      result = body["result"]
      unless result.is_a?(Hash) && !result["isError"] && result["structuredContent"].is_a?(Hash)
        raise Broker::ExchangeError.new("Mercator status unavailable", stage: "parse")
      end
      result["structuredContent"]
    rescue Broker::ExchangeError
      raise
    rescue StandardError
      raise Broker::ExchangeError.new("Mercator status unavailable", stage: "network")
    end

    def slack_claimed?(access_token, wallet:)
      body = checked_json(@http.get("#{ORIGIN}/v1/claims/slack/config",
        headers: { "Authorization" => "Bearer #{access_token}" }))
      body["wallet_address"].to_s.casecmp?(wallet) && body["claimed"] == true
    rescue StandardError
      # Status is optional UI information; Mercator still enforces claim uniqueness.
      false
    end

    private

    def checked_json(response)
      unless response.success?
        raise Broker::ExchangeError.new("Mercator request failed", stage: "http", status: response.status)
      end
      body = response.body
      if response["content-type"].to_s.start_with?("text/event-stream")
        event = body.gsub("\r\n", "\n").split("\n\n").reverse.find { |item| item.lines.any? { |line| line.start_with?("data:") } }
        body = event.to_s.lines.filter_map { |line| line.delete_prefix("data:").strip if line.start_with?("data:") }.join("\n")
      end
      value = JSON.parse(body)
      raise JSON::ParserError unless value.is_a?(Hash)
      value
    end
  end
end
