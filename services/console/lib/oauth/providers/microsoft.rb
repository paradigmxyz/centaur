module Oauth
  module Providers
    # Microsoft Entra ID consent-flow strategy for Microsoft 365 accounts. Owns
    # the v2.0 endpoints, the offline_access scope that makes Microsoft return a
    # refresh token, and how to pull a stable account identity out of the
    # id_token. Microsoft rotates the refresh token on every refresh; the broker
    # persists each rotated token before the next refresh.
    #
    # The `common` tenant endpoints accept accounts from any tenant, so the Entra
    # app registration must allow accounts outside its home tenant; a
    # single-tenant registration rejects the flow (AADSTS50194).
    class Microsoft
      include IdTokenIdentity

      KEY = "microsoft"
      AUTHORIZATION_ENDPOINT = "https://login.microsoftonline.com/common/oauth2/v2.0/authorize"
      TOKEN_ENDPOINT = "https://login.microsoftonline.com/common/oauth2/v2.0/token"
      # Always requested in addition to the app's scopes: openid/email/profile so
      # the token response carries an id_token identifying the account,
      # offline_access so it carries a refresh token. Microsoft leaves all four
      # out of the response's scope list, so they never reach the credential.
      IDENTITY_SCOPES = %w[openid email profile offline_access].freeze
      # Hosts a minted access token may be sent to. Every Microsoft Graph API is
      # served from graph.microsoft.com; login.microsoftonline.com is auth-only
      # and intentionally excluded.
      API_HOSTS = %w[graph.microsoft.com].freeze
      # Microsoft stamps the account's tenant id into the issuer.
      VALID_ISSUER = %r{\Ahttps://login\.microsoftonline\.com/[0-9a-f-]{36}/v2\.0\z}

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

      # A browser signed in to several Microsoft accounts would otherwise consent
      # silently with whichever one Microsoft picks.
      def extra_authorization_params = { "prompt" => "select_account" }

      # Extracts { subject:, email:, name: } from a successful code-exchange
      # result. The subject is the `oid` claim, the account's directory object
      # id, which is stable across apps; `sub` is pairwise per client. The email
      # is used only when `xms_edov` says its domain is verified: on the `common`
      # endpoint any tenant's admin can set an account's `email` or
      # `preferred_username` to an arbitrary address, and credential
      # reconciliation grants by email. Raises Broker::ExchangeError on a
      # missing/undecodable id_token or a mismatch.
      def identity_from(result, client_id:, http_client: nil)
        claims = id_token_claims(result)
        require_audience!(claims, client_id)
        unless VALID_ISSUER.match?(claims["iss"].to_s)
          raise Broker::ExchangeError.new("id_token iss was not a Microsoft issuer",
                                          stage: "oauth", code: "id_token_iss_invalid")
        end

        subject = claims["oid"]
        if subject.blank?
          raise Broker::ExchangeError.new("id_token carried no oid",
                                          stage: "oauth", code: "id_token_missing_oid")
        end

        {
          subject: subject,
          email: verified_email(claims),
          name: claims["name"].presence
        }
      end

      private

      def verified_email(claims)
        return unless ActiveModel::Type::Boolean.new.cast(claims["xms_edov"])

        claims["email"].presence
      end
    end
  end
end
