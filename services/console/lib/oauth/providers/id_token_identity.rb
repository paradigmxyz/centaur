require "base64"
require "json"

module Oauth
  module Providers
    # Shared id_token handling for OIDC providers whose token response carries
    # the account identity as a JWT. The payload is decoded without verifying
    # its signature: the token came directly from the IdP's token endpoint over
    # TLS, which OIDC Core 3.1.3.7.6 accepts as sufficient. Each strategy still
    # checks the issuer it expects.
    #
    # SECURITY: the id_token carries the account identity but no tokens. As
    # elsewhere under Broker/Oauth, nothing here logs token material.
    module IdTokenIdentity
      private

      # The decoded claims of +result+'s id_token. Raises Broker::ExchangeError
      # when the token is missing or its payload does not decode.
      def id_token_claims(result)
        if result.id_token.blank?
          raise Broker::ExchangeError.new("token response carried no id_token",
                                          stage: "oauth", code: "missing_id_token")
        end

        decode_id_token_claims(result.id_token)
      end

      def require_audience!(claims, client_id)
        return if claims["aud"] == client_id

        raise Broker::ExchangeError.new("id_token aud did not match client_id",
                                        stage: "oauth", code: "id_token_aud_mismatch")
      end

      # Decodes the JWT payload (second segment), tolerating the unpadded
      # base64url JWTs use.
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
