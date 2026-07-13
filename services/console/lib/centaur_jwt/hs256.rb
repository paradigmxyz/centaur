require "base64"
require "json"
require "openssl"

module CentaurJwt
  module Hs256
    module_function

    def encode(payload, signing_secret:)
      signing_secret = signing_secret.to_s
      raise KeyError, "CENTAUR_JWT_SIGNING_SECRET is not configured" if signing_secret.blank?

      header = { "alg" => "HS256", "typ" => "JWT" }
      signing_input = [ base64url_json(header), base64url_json(payload) ].join(".")
      signature = OpenSSL::HMAC.digest("SHA256", signing_secret, signing_input)
      "#{signing_input}.#{Base64.urlsafe_encode64(signature, padding: false)}"
    end

    def base64url_json(value)
      Base64.urlsafe_encode64(JSON.generate(value), padding: false)
    end
  end
end
