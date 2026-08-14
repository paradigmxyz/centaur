module ApiServer
  module Jwt
    DEFAULT_AUDIENCE = "centaur-api".freeze
    DEFAULT_ISSUER = "centaur-console".freeze
    DEFAULT_WINDOW_SECONDS = 15.minutes.to_i
    DEFAULT_TTL_SECONDS = 1.hour.to_i
    CONSOLE_SERVICE_SUBJECT = "centaur-console".freeze

    module_function

    def encode_for_principal(principal, now: Time.current)
      channels = principal.slack_channel_ids_by_permission
      upload_channels = channels.fetch(:upload)
      download_channels = channels.fetch(:download)
      history_channels = channels.fetch(:history)

      CentaurJwt::WindowedToken.encode(
        subject_oid: principal.oid,
        audience: audience,
        issuer: issuer,
        window_seconds: DEFAULT_WINDOW_SECONDS,
        ttl_seconds: DEFAULT_TTL_SECONDS,
        now: now,
        claims: {
          "sub" => principal.oid,
          "capabilities" => {
            "sessions_read" => principal.sandbox_sessions_read_enabled,
            "workflows_read" => principal.sandbox_workflows_read_enabled,
            "workflows_write" => principal.sandbox_workflows_write_enabled
          },
          "slack" => {
            "upload_channels" => upload_channels,
            "download_channels" => download_channels,
            "history_channels" => history_channels
          }
        }
      )
    end

    def encode_for_console_service(now: Time.current)
      CentaurJwt::WindowedToken.encode(
        subject_oid: CONSOLE_SERVICE_SUBJECT,
        audience: audience,
        issuer: issuer,
        window_seconds: DEFAULT_WINDOW_SECONDS,
        ttl_seconds: DEFAULT_TTL_SECONDS,
        now: now,
        claims: {
          "sub" => CONSOLE_SERVICE_SUBJECT,
          "token_use" => "console_service"
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
