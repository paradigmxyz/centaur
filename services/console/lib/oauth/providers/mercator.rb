module Oauth
  module Providers
    # Wallet consent and payment signing remain on Mercator/Tempo Wallet.
    class Mercator
      KEY = "mercator"
      ORIGIN = "https://mercator.sh"
      SCOPE = "mercator:tools"

      def key = KEY
      def display_name = "Mercator"
      def authorization_endpoint = "#{ORIGIN}/authorize"
      def token_endpoint = "#{ORIGIN}/oauth/token"
      def identity_scopes = []
      def required_scopes = [ SCOPE ]
      def api_hosts = [ "mercator.sh" ]
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def extra_authorization_params = { "resource" => "#{ORIGIN}/mcp/auth" }
      def refreshable? = true
      def parse_granted_scopes(scope) = scope.to_s.split
      def refresh_scopes(scopes) = Array(scopes)

      def validate_result!(result)
        return if result.scope.blank? || parse_granted_scopes(result.scope).include?(SCOPE)
        raise Broker::ExchangeError.new("Mercator scope was not granted", stage: "oauth", code: "missing_scope")
      end

      def identity_from(result, client_id:, http_client:)
        status = ::Mercator::ConnectionClient.new(http: http_client).status(result.access_token)
        address = status.dig("account", "walletAddress")
        unless status["oauthAuthenticated"] == true && address.is_a?(String) && address.match?(/\A0x[0-9a-fA-F]{40}\z/)
          raise Broker::ExchangeError.new("Mercator wallet identity unavailable", stage: "oauth", code: "missing_wallet")
        end
        { subject: address.downcase, name: address, labels: { "centaur-tool" => KEY } }
      end
    end
  end
end
