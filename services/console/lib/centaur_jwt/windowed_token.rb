require "zlib"

module CentaurJwt
  # Tokens that rotate on a fixed window rather than per request, so the token
  # bytes — and any synced proxy config that embeds them — stay stable within
  # a window and the sync hash short-circuit keeps working. Rotation
  # boundaries are offset per subject (deterministically, from its oid) so a
  # fleet's tokens don't all roll over — and force config re-pushes — at the
  # same instant.
  module WindowedToken
    module_function

    # Returns nil when the signing secret is unconfigured, matching the
    # callers' contract of quietly omitting the token from synced config.
    def encode(subject_oid:, audience:, issuer:, window_seconds:, ttl_seconds:, claims:, now: Time.current)
      signing_secret = ENV["CENTAUR_JWT_SIGNING_SECRET"].to_s
      return nil if signing_secret.blank?

      issued_at = window_start(subject_oid, now.to_i, window_seconds: window_seconds)
      CentaurJwt::Hs256.encode(
        {
          "iss" => issuer,
          "aud" => audience,
          "iat" => issued_at,
          "exp" => issued_at + ttl_seconds
        }.merge(claims),
        signing_secret: signing_secret
      )
    end

    def window_start(oid, timestamp, window_seconds:)
      offset = rotation_offset(oid, window_seconds: window_seconds)
      timestamp - ((timestamp - offset) % window_seconds)
    end

    def rotation_offset(oid, window_seconds:)
      Zlib.crc32(oid.to_s) % window_seconds
    end
  end
end
