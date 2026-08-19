require "jwt"

module CentaurJwt
  module Hs256
    VerificationError = Class.new(StandardError)

    module_function

    def encode(payload, signing_secret:)
      signing_secret = signing_secret.to_s
      raise KeyError, "CENTAUR_JWT_SIGNING_SECRET is not configured" if signing_secret.blank?

      JWT.encode(payload, signing_secret, "HS256", { "typ" => "JWT" })
    end

    def decode(token, signing_secret:, aud: nil, iss: nil)
      signing_secret = signing_secret.to_s
      raise KeyError, "CENTAUR_JWT_SIGNING_SECRET is not configured" if signing_secret.blank?

      payload, _header = JWT.decode(
        token.to_s,
        signing_secret,
        true,
        algorithm: "HS256",
        verify_iss: iss.present?,
        iss: iss,
        verify_aud: aud.present?,
        aud: aud
      )
      payload
    rescue JWT::DecodeError => e
      raise VerificationError, e.message
    end
  end
end
