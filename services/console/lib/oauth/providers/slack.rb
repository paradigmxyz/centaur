module Oauth
  module Providers
    # Slack user-token consent-flow strategy. Uses Slack's standard OAuth v2
    # authorize/access endpoints with user_scope, so the broker stores the nested
    # authed_user token returned by Slack.
    class Slack
      KEY = "slack"
      AUTHORIZATION_ENDPOINT = "https://slack.com/oauth/v2/authorize"
      TOKEN_ENDPOINT = "https://slack.com/api/oauth.v2.access"
      # Do not add Sign in with Slack scopes here. Slack rejects requests that
      # mix SIWS scopes with normal API scopes such as channels:history.
      IDENTITY_SCOPES = [].freeze
      API_HOSTS = %w[slack.com].freeze
      VALID_ISSUERS = %w[https://slack.com].freeze

      def key = KEY
      def display_name = "Slack"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "user_scope"
      def scope_separator = ","
      def extra_authorization_params = {}
      def refreshable? = true
      # Slack app token rotation is deployment-specific. When token rotation is
      # enabled Slack returns a refresh_token and the broker loop keeps it fresh;
      # when it is disabled Slack returns long-lived xoxp/xoxb tokens without a
      # refresh_token, which should be stored without scheduling refresh.
      def require_refresh_token? = false
      def refreshable_result?(result) = result.refresh_token.present?

      def validate_result!(result)
        return unless result.refresh_token.blank? && result.expires_in.present?

        raise Broker::ExchangeError.new(
          "token endpoint returned expiring Slack token without refresh_token",
          stage: "oauth",
          code: "missing_refresh_token"
        )
      end

      def parse_granted_scopes(scope)
        scope.to_s.split(/[,\s]+/).reject(&:blank?)
      end

      def refresh_scopes(_scopes) = []

      def identity_from(result, client_id:)
        user_id = result.response&.dig("authed_user", "id")
        if user_id.present?
          return {
            subject: user_id,
            email: result.response.dig("authed_user", "email"),
            name: slack_user_name(result.response),
            team_id: slack_team_id(result.response)
          }
        end

        bot_user_id = result.response&.dig("bot_user_id")
        if bot_user_id.present?
          return {
            subject: bot_user_id,
            email: nil,
            name: slack_bot_name(result.response),
            team_id: slack_team_id(result.response)
          }
        end

        Login::IdToken.identity(result.id_token, client_id: client_id,
                                                 valid_issuers: VALID_ISSUERS)
                      .slice(:subject, :email, :name)
      end

      private

      def slack_user_name(response)
        response.dig("authed_user", "name").presence ||
          response.dig("authed_user", "user").presence
      end

      def slack_team_id(response)
        response.dig("team", "id").presence ||
          response.dig("authed_user", "team_id").presence
      end

      def slack_bot_name(response)
        response.dig("team", "name").presence || response["bot_user_id"].presence
      end
    end
  end
end
