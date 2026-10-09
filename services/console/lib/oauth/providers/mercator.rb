module Oauth
  module Providers
    class Mercator
      include HttpIdentity

      KEY = "mercator"
      ORIGIN = "https://mercator.sh"
      SCOPE = "mercator:tools"

      def key = KEY
      def display_name = "Mercator"
      def authorization_endpoint = "#{ORIGIN}/authorize"
      def token_endpoint = "#{ORIGIN}/oauth/token"
      def registration_endpoint = "#{ORIGIN}/oauth/register"
      def identity_scopes = []
      def required_scopes = [ SCOPE ]
      def api_hosts = [ "mercator.sh" ]
      def credential_request_rules
        [ { host: "mercator.sh", http_methods: [ "POST" ], paths: [ "/mcp/auth" ] } ]
      end
      def preset
        { slug: KEY, description: "Shared with your agents.",
          allowed_scopes: [ SCOPE ], shared: true }
      end
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def extra_authorization_params = { "resource" => "#{ORIGIN}/mcp/auth" }
      def refreshable? = true
      def parse_granted_scopes(scope) = scope.to_s.split
      def refresh_scopes(scopes) = Array(scopes)

      def validate_result!(result)
        return if parse_granted_scopes(result.scope).include?(SCOPE)
        raise Broker::ExchangeError.new("Mercator scope was not granted", stage: "oauth", code: "missing_scope")
      end

      def identity_from(result, client_id:, http_client: HttpClient.new)
        if result.access_token.blank?
          raise Broker::ExchangeError.new("Missing access token", stage: "parse", code: "missing_access_token")
        end
        response = identity_response(provider: display_name) do
          http_client.post("#{ORIGIN}/mcp/auth",
            headers: { "Authorization" => "Bearer #{result.access_token}", "Accept" => "application/json, text/event-stream" },
            json: { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "get_connection_status", arguments: {} } })
        end
        if response["content-type"].to_s.start_with?("text/event-stream")
          event = response.body.to_s.gsub("\r\n", "\n").split("\n\n").reverse.find { |item| item.lines.any? { |line| line.start_with?("data:") } }
          body = event.to_s.lines.filter_map { |line| line.delete_prefix("data:").strip if line.start_with?("data:") }.join("\n")
          response = HttpClient::Response.new(status: response.status, body: body)
        end
        payload = identity_json(response, provider: display_name)
        result = payload["result"]
        status = result["structuredContent"] if result.is_a?(Hash) && !result["isError"] && !payload["error"]
        account = status["account"] if status.is_a?(Hash)
        address = account["walletAddress"] if account.is_a?(Hash)
        unless status.is_a?(Hash) && status["oauthAuthenticated"] == true && address.is_a?(String) && address.match?(/\A0x[0-9a-fA-F]{40}\z/)
          raise Broker::ExchangeError.new("Mercator wallet identity unavailable", stage: "oauth", code: "missing_wallet")
        end
        { subject: address.downcase, name: address }
      end

      def details_for(credential, http_client:)
        response = http_client.get("#{ORIGIN}/v1/claims/config?provider=slack",
          headers: { "Authorization" => "Bearer #{credential.access_token}" })
        body = response.json if response.success?
        unless body.is_a?(Hash) && body["wallet_address"].is_a?(String) &&
            body["wallet_address"].casecmp?(credential.provider_subject.to_s) && [ true, false ].include?(body["claimed"])
          return { claimed: nil }
        end
        { claimed: body["claimed"] }
      rescue StandardError
        { claimed: nil }
      end
    end
  end
end
