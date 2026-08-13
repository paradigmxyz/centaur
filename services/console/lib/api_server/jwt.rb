module ApiServer
  module Jwt
    DEFAULT_AUDIENCE = "centaur-api".freeze
    DEFAULT_ISSUER = "centaur-console".freeze
    DEFAULT_WINDOW_SECONDS = 15.minutes.to_i
    DEFAULT_TTL_SECONDS = 1.hour.to_i

    module_function

    def encode_for_principal(principal, now: Time.current)
      channels = principal.slack_channel_ids_by_permission
      upload_channels = channels.fetch(:upload)
      download_channels = channels.fetch(:download)
      history_channels = channels.fetch(:history)
      return nil if upload_channels.empty? && download_channels.empty? && history_channels.empty?

      CentaurJwt::WindowedToken.encode(
        subject_oid: principal.oid,
        audience: audience,
        issuer: issuer,
        window_seconds: DEFAULT_WINDOW_SECONDS,
        ttl_seconds: DEFAULT_TTL_SECONDS,
        now: now,
        claims: {
          "sub" => principal.oid,
          "slack" => {
            "upload_channels" => upload_channels,
            "download_channels" => download_channels,
            "history_channels" => history_channels
          }
        }
      )
    end

    def encode_for_console_user(user, admin: false, now: Time.current)
      development_principal_ids = user.user_identities.feishu.filter_map do |identity|
        next if identity.tenant_key.blank? || identity.open_id.blank?
        "feishu:#{identity.tenant_key}:#{identity.open_id}"
      end.uniq.sort.first(16)
      CentaurJwt::WindowedToken.encode(
        subject_oid: user.oid,
        audience: audience,
        issuer: issuer,
        window_seconds: DEFAULT_WINDOW_SECONDS,
        ttl_seconds: DEFAULT_TTL_SECONDS,
        now: now,
        claims: {
          "sub" => user.oid,
          "centaur_admin" => admin == true,
          "development_principal_ids" => development_principal_ids
        }
      )
    end

    # Kept for callers that reason about rotation boundaries directly
    # (snapshot staleness checks, tests).
    def window_start_for(principal, timestamp)
      CentaurJwt::WindowedToken.window_start(principal.oid, timestamp, window_seconds: DEFAULT_WINDOW_SECONDS)
    end

    def rotation_offset(principal)
      CentaurJwt::WindowedToken.rotation_offset(principal.oid, window_seconds: DEFAULT_WINDOW_SECONDS)
    end

    def audience
      ENV["CENTAUR_API_JWT_AUDIENCE"].presence || DEFAULT_AUDIENCE
    end

    def issuer
      ENV["CENTAUR_API_JWT_ISSUER"].presence || DEFAULT_ISSUER
    end
  end
end
