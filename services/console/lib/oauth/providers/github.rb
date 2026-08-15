module Oauth
  module Providers
    # GitHub OAuth App consent-flow strategy. GitHub's OAuth App token response
    # carries the access token and scopes but no account identity. Resolve the
    # authenticated user synchronously so the callback can upsert by the stable
    # GitHub user id instead of creating a token-derived pending credential.
    class Github
      include HttpIdentity

      KEY = "github"
      AUTHORIZATION_ENDPOINT = "https://github.com/login/oauth/authorize"
      TOKEN_ENDPOINT = "https://github.com/login/oauth/access_token"
      USER_ENDPOINT = "https://api.github.com/user"
      IDENTITY_SCOPES = [].freeze

      def key = KEY
      def display_name = "GitHub"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def extra_authorization_params = {}
      def refreshable? = false

      def wrapping_secret_kind = CredentialProfiles::GithubToken::KIND

      def parse_granted_scopes(scope)
        scope.to_s.split(/[,\s]+/).reject(&:blank?)
      end

      def refresh_scopes(_scopes) = []

      def identity_from(result, client_id:, http_client: HttpClient.new)
        if result.access_token.blank?
          raise Broker::ExchangeError.new("token response returned an empty access_token",
                                          stage: "parse", code: "missing_access_token")
        end

        response = identity_response(provider: display_name) do
          http_client.get(
            USER_ENDPOINT,
            headers: {
              "Accept" => "application/vnd.github+json",
              "Authorization" => "Bearer #{result.access_token}",
              "X-GitHub-Api-Version" => "2022-11-28",
              "User-Agent" => "centaur-console"
            }
          )
        end
        profile = identity_json(response, provider: display_name)
        subject = require_identity(profile["id"], provider: display_name).to_s
        login = require_identity(profile["login"], provider: display_name).to_s

        {
          subject: subject,
          email: profile["email"].presence,
          name: profile["name"].presence || login,
          labels: { "github_login" => login }
        }
      end
    end
  end
end
