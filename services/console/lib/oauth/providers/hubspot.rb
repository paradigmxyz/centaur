require "digest"

module Oauth
  module Providers
    # HubSpot OAuth consent-flow strategy. HubSpot's token response carries
    # access/refresh tokens and scopes but no account identity. To keep the
    # callback path free of external API calls, the flow stores a deterministic
    # pending identity derived from the token and
    # EnrichHubspotCredentialIdentityJob replaces it with the HubSpot portal and
    # user ids from the access-token metadata endpoint.
    class Hubspot
      KEY = "hubspot"
      AUTHORIZATION_ENDPOINT = "https://app.hubspot.com/oauth/authorize"
      TOKEN_ENDPOINT = "https://api.hubapi.com/oauth/v3/token"
      ACCESS_TOKEN_INFO_ENDPOINT = "https://api.hubapi.com/oauth/v1/access-tokens"
      IDENTITY_SCOPES = [].freeze
      API_HOSTS = %w[api.hubapi.com api.hubspot.com].freeze

      def key = KEY
      def display_name = "HubSpot"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def extra_authorization_params = {}
      def refreshable? = true

      def parse_granted_scopes(scope)
        scope.to_s.split(/[,\s]+/).reject(&:blank?)
      end

      def refresh_scopes(scopes) = Array(scopes)

      def identity_from(result, client_id:)
        if result.access_token.blank?
          raise Broker::ExchangeError.new("token response returned an empty access_token",
                                          stage: "parse", code: "missing_access_token")
        end

        {
          subject: "pending-#{Digest::SHA256.hexdigest(result.access_token)[0, 32]}",
          email: nil,
          name: "Pending HubSpot account"
        }
      end
    end
  end
end
