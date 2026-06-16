require "base64"
require "json"

module Login
  # Shared OIDC id_token handling for the console-login provider strategies.
  #
  # SECURITY: like Oauth::Providers::Google, the id_token came directly from the
  # provider's token endpoint over TLS, which OIDC Core 3.1.3.7.6 accepts without
  # a separate signature check. We still verify aud == client_id and iss against
  # the provider's known issuers. Nothing here logs token material.
  module IdToken
    module_function

    # Returns { subject:, email:, email_verified:, name: } from a code-exchange
    # result's id_token. Raises Broker::ExchangeError (the same error the exchange
    # itself raises, so the controller has one rescue) on any problem.
    def identity(id_token, client_id:, valid_issuers:)
      if id_token.blank?
        raise Broker::ExchangeError.new("token response carried no id_token",
                                        stage: "oauth", code: "missing_id_token")
      end

      claims = decode_claims(id_token)

      unless claims["aud"] == client_id
        raise Broker::ExchangeError.new("id_token aud did not match client_id",
                                        stage: "oauth", code: "id_token_aud_mismatch")
      end
      unless valid_issuers.include?(claims["iss"])
        raise Broker::ExchangeError.new("id_token iss was not an expected issuer",
                                        stage: "oauth", code: "id_token_iss_invalid")
      end

      subject = claims["sub"]
      if subject.blank?
        raise Broker::ExchangeError.new("id_token carried no sub",
                                        stage: "oauth", code: "id_token_missing_sub")
      end

      {
        subject: subject,
        email: claims["email"],
        # Absent/false email_verified blocks email-based account linking.
        email_verified: claims["email_verified"] == true,
        name: claims["name"].presence
      }
    end

    # Decodes the JWT payload (second segment), tolerating the unpadded base64url
    # JWTs use. No signature verification -- see the module note.
    def decode_claims(id_token)
      seg = id_token.split(".")[1].to_s
      seg += "=" * ((4 - seg.length % 4) % 4)
      JSON.parse(Base64.urlsafe_decode64(seg))
    rescue ArgumentError, JSON::ParserError
      raise Broker::ExchangeError.new("id_token payload did not decode", stage: "parse")
    end
  end
end
