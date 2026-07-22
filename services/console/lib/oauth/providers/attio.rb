require "digest"

module Oauth
  module Providers
    # Attio OAuth consent-flow strategy. Attio app scopes are configured in the
    # Attio developer dashboard, not requested on the authorization redirect;
    # the token response carries a long-lived workspace-scoped access token with
    # no refresh token, expiry, scope, or identity payload. The callback stores a
    # deterministic pending workspace identity derived from the token, and
    # EnrichAttioCredentialIdentityJob replaces it with the Attio workspace id
    # and name from /v2/self.
    class Attio
      KEY = "attio"
      AUTHORIZATION_ENDPOINT = "https://app.attio.com/authorize"
      TOKEN_ENDPOINT = "https://app.attio.com/oauth/token"
      SELF_ENDPOINT = "https://api.attio.com/v2/self"
      IDENTITY_SCOPES = [].freeze
      API_HOSTS = %w[api.attio.com].freeze

      def key = KEY
      def display_name = "Attio"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def extra_authorization_params = {}
      def refreshable? = false

      def parse_granted_scopes(scope)
        scope.to_s.split(/[,\s]+/).reject(&:blank?)
      end

      def refresh_scopes(_scopes) = []

      def identity_from(result, client_id:)
        if result.access_token.blank?
          raise Broker::ExchangeError.new("token response returned an empty access_token",
                                          stage: "parse", code: "missing_access_token")
        end

        {
          subject: "pending-#{Digest::SHA256.hexdigest(result.access_token)[0, 32]}",
          email: nil,
          name: "Pending Attio workspace"
        }
      end
    end
  end
end
