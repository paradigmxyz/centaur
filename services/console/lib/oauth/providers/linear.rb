require "digest"

module Oauth
  module Providers
    # Linear OAuth consent-flow strategy. Linear's token response carries the
    # access token, granted scopes, and rotating refresh token but no account
    # identity. To keep the callback path free of external API calls, the flow
    # stores a deterministic pending identity derived from the token and
    # EnrichLinearCredentialIdentityJob replaces it with the authenticated
    # Linear viewer id/name/email.
    class Linear
      KEY = "linear"
      AUTHORIZATION_ENDPOINT = "https://linear.app/oauth/authorize"
      TOKEN_ENDPOINT = "https://api.linear.app/oauth/token"
      GRAPHQL_ENDPOINT = "https://api.linear.app/graphql"
      IDENTITY_SCOPES = [].freeze
      API_HOSTS = %w[api.linear.app].freeze

      def key = KEY
      def display_name = "Linear"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "scope"
      def scope_separator = ","
      def extra_authorization_params = {}
      def refreshable? = true

      def parse_granted_scopes(scope)
        case scope
        when Array
          scope.map(&:to_s).reject(&:blank?)
        else
          scope.to_s.split(/[,\s]+/).reject(&:blank?)
        end
      end

      # Linear accepts an optional scope parameter on refresh. Pass through the
      # originally granted scopes so a refresh preserves the consented grant set;
      # CredentialGrants serializes this as the token endpoint's space-separated
      # scope field.
      def refresh_scopes(scopes) = Array(scopes)

      def identity_from(result, client_id:)
        if result.access_token.blank?
          raise Broker::ExchangeError.new("token response returned an empty access_token",
                                          stage: "parse", code: "missing_access_token")
        end

        {
          subject: "pending-#{Digest::SHA256.hexdigest(result.access_token)[0, 32]}",
          email: nil,
          name: "Pending Linear account"
        }
      end
    end
  end
end
