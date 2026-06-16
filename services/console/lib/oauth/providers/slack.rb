require "json"
require "net/http"
require "uri"

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
      AUTH_TEST_ENDPOINT = "https://slack.com/api/auth.test"
      USERS_INFO_ENDPOINT = "https://slack.com/api/users.info"

      class << self
        attr_accessor :slack_api_http
      end

      def key = KEY
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "user_scope"
      def scope_separator = ","
      def extra_authorization_params = {}

      def parse_granted_scopes(scope)
        scope.to_s.split(/[,\s]+/).reject(&:blank?)
      end

      def refresh_scopes(_scopes) = []

      def identity_from(result, client_id:)
        user_id = result.response&.dig("authed_user", "id")
        if user_id.present?
          profile = slack_profile(result.access_token, user_id, result.scope)
          return {
            subject: user_id,
            email: result.response.dig("authed_user", "email").presence || profile[:email],
            name: slack_user_name(result.response).presence || profile[:name]
          }
        end

        Login::IdToken.identity(result.id_token, client_id: client_id,
                                                 valid_issuers: VALID_ISSUERS)
                      .slice(:subject, :email, :name)
      end

      private

      def slack_profile(access_token, user_id, scope)
        profile = {}
        profile[:name] = auth_test_user(access_token)
        return profile unless parse_granted_scopes(scope).include?("users:read")

        info = slack_api(USERS_INFO_ENDPOINT, access_token, "user" => user_id)
        return profile unless info.is_a?(Hash) && info["ok"] == true

        user = info["user"].is_a?(Hash) ? info["user"] : {}
        user_profile = user["profile"].is_a?(Hash) ? user["profile"] : {}
        profile[:name] = user_profile["display_name"].presence ||
                         user_profile["real_name"].presence ||
                         user["real_name"].presence ||
                         user["name"].presence ||
                         profile[:name]
        if parse_granted_scopes(scope).include?("users:read.email")
          profile[:email] = user_profile["email"].presence
        end
        profile
      rescue StandardError
        profile
      end

      def slack_user_name(response)
        response.dig("authed_user", "name").presence ||
          response.dig("authed_user", "user").presence
      end

      def auth_test_user(access_token)
        response = slack_api(AUTH_TEST_ENDPOINT, access_token)
        return nil unless response.is_a?(Hash) && response["ok"] == true
        response["user"].presence
      end

      def slack_api(url, access_token, params = {})
        return nil if access_token.blank?

        if self.class.slack_api_http
          return self.class.slack_api_http.call(url: url, access_token: access_token, params: params)
        end

        uri = URI.parse(url)
        req = Net::HTTP::Post.new(uri)
        req["Authorization"] = "Bearer #{access_token}"
        req["Accept"] = "application/json"
        req.set_form_data(params) if params.any?

        http = Net::HTTP.new(uri.host, uri.port)
        http.use_ssl = uri.scheme == "https"
        http.open_timeout = 5
        http.read_timeout = 5

        JSON.parse(http.request(req).body.to_s)
      end
    end
  end
end
