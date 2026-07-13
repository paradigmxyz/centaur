module Mcp
  module Jwt
    module_function

    def encode(payload)
      signing_secret = ENV["CENTAUR_JWT_SIGNING_SECRET"].to_s
      CentaurJwt::Hs256.encode(payload, signing_secret: signing_secret)
    end
  end
end
