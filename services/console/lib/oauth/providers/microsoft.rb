require "base64"
require "json"

module Oauth
  module Providers
    # Microsoft identity platform (Entra ID) consent-flow strategy for Graph API
    # access. Uses the v2 endpoints with the "common" tenant so multi-tenant apps
    # work out of the box; single-tenant apps should register a matching
    # authorize/token host in their Entra app and can still use this strategy as
    # long as the id_token issuer matches login.microsoftonline.com.
    #
    # SECURITY: identity extraction touches the id_token, which carries the
    # account identity but no tokens. As elsewhere under Broker/Oauth, nothing
    # here logs token material.
    class Microsoft
      KEY = "microsoft"
      TENANT = "common"
      AUTHORIZATION_ENDPOINT =
        "https://login.microsoftonline.com/#{TENANT}/oauth2/v2.0/authorize"
      TOKEN_ENDPOINT =
        "https://login.microsoftonline.com/#{TENANT}/oauth2/v2.0/token"
      # Always requested so the token response carries an id_token and a refresh
      # token. Graph API scopes come from the OauthApp allowed_scopes list.
      IDENTITY_SCOPES = %w[openid profile email offline_access].freeze
      API_HOSTS = %w[graph.microsoft.com].freeze
      VALID_ISSUER_PATTERN =
        %r{\Ahttps://login\.microsoftonline\.com/[0-9a-fA-F-]+/v2\.0\z}

      def key = KEY
      def display_name = "Microsoft"
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def identity_scopes = IDENTITY_SCOPES
      def api_hosts = API_HOSTS
      def authorization_scope_param = "scope"
      def scope_separator = " "
      def refreshable? = true

      def parse_granted_scopes(scope) = scope.to_s.split
      def refresh_scopes(scopes) = Array(scopes)

      # prompt=consent forces a fresh refresh token on re-consent, matching the
      # Google strategy's offline guarantee.
      def extra_authorization_params = { "prompt" => "consent" }

      def identity_from(result, client_id:)
        if result.id_token.blank?
          raise Broker::ExchangeError.new("token response carried no id_token",
                                          stage: "oauth", code: "missing_id_token")
        end

        claims = decode_id_token_claims(result.id_token)

        unless audience_matches?(claims["aud"], client_id)
          raise Broker::ExchangeError.new("id_token aud did not match client_id",
                                          stage: "oauth", code: "id_token_aud_mismatch")
        end
        unless VALID_ISSUER_PATTERN.match?(claims["iss"].to_s)
          raise Broker::ExchangeError.new("id_token iss was not a Microsoft issuer",
                                          stage: "oauth", code: "id_token_iss_invalid")
        end

        subject = claims["sub"]
        if subject.blank?
          raise Broker::ExchangeError.new("id_token carried no sub",
                                          stage: "oauth", code: "id_token_missing_sub")
        end

        email = claims["email"].presence || claims["preferred_username"].presence
        { subject: subject, email: email }
      end

      private

      def audience_matches?(aud, client_id)
        case aud
        when String then aud == client_id
        when Array then aud.include?(client_id)
        else false
        end
      end

      def decode_id_token_claims(id_token)
        seg = id_token.split(".")[1].to_s
        seg += "=" * ((4 - seg.length % 4) % 4)
        JSON.parse(Base64.urlsafe_decode64(seg))
      rescue ArgumentError, JSON::ParserError
        raise Broker::ExchangeError.new("id_token payload did not decode", stage: "parse")
      end
    end
  end
end
