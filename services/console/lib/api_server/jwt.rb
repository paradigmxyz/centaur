require "zlib"

module ApiServer
  module Jwt
    DEFAULT_AUDIENCE = "centaur-api".freeze
    DEFAULT_ISSUER = "centaur-console".freeze
    DEFAULT_WINDOW_SECONDS = 15.minutes.to_i
    DEFAULT_TTL_SECONDS = 1.hour.to_i

    module_function

    def encode_for_principal(principal, now: Time.current)
      channel_id = principal.labels.to_h[Principal::SLACK_CHANNEL_ID_LABEL].to_s.strip
      return nil if channel_id.blank?

      signing_secret = ENV["CENTAUR_JWT_SIGNING_SECRET"].to_s
      return nil if signing_secret.blank?

      issued_at = window_start_for(principal, now.to_i)
      expires_at = issued_at + DEFAULT_TTL_SECONDS
      slack_claims = {
        "upload_channels" => [ channel_id ],
        "download_channels" => [ channel_id ]
      }
      search_channel = slack_search_channel_name(principal)
      slack_claims["search_channels"] = [ search_channel ] if search_channel.present?
      CentaurJwt::Hs256.encode(
        {
          "iss" => issuer,
          "sub" => principal.oid,
          "aud" => audience,
          "iat" => issued_at,
          "exp" => expires_at,
          "slack" => slack_claims
        },
        signing_secret: signing_secret
      )
    end

    # Rotation boundaries are offset per principal (deterministically, from
    # the oid) so the fleet's tokens don't all roll over — and force snapshot
    # rebuilds — at the same instant.
    def window_start_for(principal, timestamp)
      offset = rotation_offset(principal)
      timestamp - ((timestamp - offset) % DEFAULT_WINDOW_SECONDS)
    end

    def rotation_offset(principal)
      Zlib.crc32(principal.oid.to_s) % DEFAULT_WINDOW_SECONDS
    end

    def audience
      ENV["CENTAUR_API_JWT_AUDIENCE"].presence || DEFAULT_AUDIENCE
    end

    def issuer
      ENV["CENTAUR_API_JWT_ISSUER"].presence || DEFAULT_ISSUER
    end

    def slack_search_channel_name(principal)
      labels = principal.labels.to_h
      explicit = labels[Principal::SLACK_CHANNEL_NAME_LABEL].to_s.strip.delete_prefix("#")
      return explicit.downcase if slack_channel_name?(explicit)

      match = principal.name.to_s.match(/\ASlack Channel #(?<name>[A-Za-z0-9_-]+)\z/)
      name = match && match[:name].to_s
      return name.downcase if slack_channel_name?(name)

      nil
    end

    def slack_channel_name?(value)
      value.to_s.match?(/\A[A-Za-z0-9_-]+\z/)
    end
  end
end
