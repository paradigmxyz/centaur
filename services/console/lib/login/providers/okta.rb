require "jwt"

module Login
  module Providers
    # Okta console login through any OIDC authorization server. The configured
    # issuer may be the organization issuer or a custom authorization-server
    # issuer; discovery supplies the endpoints and JWKS used for verification.
    class Okta
      KEY = "okta"
      SCOPES = %w[openid email profile].freeze
      ALGORITHMS = %w[RS256].freeze
      REQUIRED_CLAIMS = %w[iss aud exp iat sub nonce].freeze
      TOKEN_ENDPOINT_AUTH_METHODS = %w[client_secret_basic client_secret_post].freeze
      MAX_USERINFO_BYTES = 64.kilobytes

      def key = KEY
      def authorization_endpoint = metadata.fetch("authorization_endpoint")
      def token_endpoint = metadata.fetch("token_endpoint")
      def scopes = SCOPES
      def extra_authorization_params = {}
      def pkce? = true
      def token_exchange_client_secret(secret) = secret

      def token_endpoint_auth_method
        method = ConsoleAuth.token_endpoint_auth_method(KEY)
        return method if TOKEN_ENDPOINT_AUTH_METHODS.include?(method)

        raise exchange_error("Okta token endpoint authentication method is invalid", "token_endpoint_auth_method_invalid")
      end

      def identity_from(result, client_id:, nonce:)
        claims = verified_claims(result.id_token, client_id: client_id)
        unless ActiveSupport::SecurityUtils.secure_compare(claims["nonce"].to_s, nonce.to_s)
          raise exchange_error("id_token nonce did not match login flow", "id_token_nonce_mismatch")
        end

        profile = userinfo(result.access_token)
        unless profile["sub"].present? && profile["sub"] == claims.fetch("sub")
          raise exchange_error("UserInfo subject did not match id_token", "userinfo_subject_mismatch")
        end
        unless profile["email"].present? && profile["email_verified"] == true
          raise exchange_error("Okta account must have a verified email", "userinfo_email_unverified")
        end

        {
          subject: claims.fetch("sub"),
          email: profile.fetch("email"),
          email_verified: true,
          name: profile["name"].presence || claims["name"].presence
        }
      end

      private

      def issuer = Login::OidcDiscovery.normalized_issuer(ConsoleAuth.issuer(KEY))
      def metadata = Login::OidcDiscovery.metadata(issuer)

      def verified_claims(id_token, client_id:)
        if id_token.blank?
          raise exchange_error("token response carried no id_token", "missing_id_token")
        end

        jwks_loader = lambda do |options|
          Login::OidcDiscovery.jwks(metadata, force: options[:invalidate])
        end
        JWT.decode(
          id_token,
          nil,
          true,
          algorithms: ALGORITHMS,
          jwks: jwks_loader,
          iss: issuer,
          verify_iss: true,
          aud: client_id,
          verify_aud: true,
          verify_iat: true,
          required_claims: REQUIRED_CLAIMS
        ).first
      rescue JWT::DecodeError => e
        raise exchange_error("id_token verification failed: #{e.class.name}", "id_token_invalid")
      end

      def userinfo(access_token)
        if access_token.blank?
          raise exchange_error("token response carried no access_token", "missing_access_token")
        end

        response = begin
          HttpClient.new(max_body_bytes: MAX_USERINFO_BYTES).get(
            metadata.fetch("userinfo_endpoint"),
            headers: { "Authorization" => "Bearer #{access_token}" }
          )
        rescue StandardError
          raise exchange_error("Okta UserInfo request failed", "userinfo_request_failed")
        end
        unless response.success?
          raise exchange_error("Okta UserInfo request failed", "userinfo_request_failed")
        end
        profile = response.json
        unless profile.is_a?(Hash)
          raise exchange_error("Okta UserInfo response was not a JSON object", "userinfo_invalid")
        end
        profile
      rescue JSON::ParserError
        raise exchange_error("Okta UserInfo response was not valid JSON", "userinfo_invalid")
      end

      def exchange_error(...) = Login::OidcDiscovery.exchange_error(...)
    end
  end
end
