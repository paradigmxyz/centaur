require "digest"

module Oauth
  module Providers
    # GitHub OAuth App consent-flow strategy. GitHub's OAuth App token response
    # carries the access token and scopes but no account identity. To keep the
    # callback path free of external API calls, the flow stores a deterministic
    # pending identity derived from the token and EnrichGithubCredentialIdentityJob
    # replaces it with the authenticated GitHub user id/name/email.
    class Github
      KEY = "github"
      AUTHORIZATION_ENDPOINT = "https://github.com/login/oauth/authorize"
      TOKEN_ENDPOINT = "https://github.com/login/oauth/access_token"
      USER_ENDPOINT = "https://api.github.com/user"
      IDENTITY_SCOPES = [].freeze
      API_HOSTS = %w[api.github.com github.com].freeze

      def key = KEY
      def display_name = "GitHub"
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
          name: "Pending GitHub account"
        }
      end
    end
  end
end
