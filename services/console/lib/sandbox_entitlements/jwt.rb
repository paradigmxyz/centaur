module SandboxEntitlements
  module Jwt
    DEFAULT_AUDIENCE = "centaur-console-sandbox-entitlements".freeze
    DEFAULT_ISSUER = "centaur-console".freeze
    # The token is low-sensitivity (scoped to read-only sandbox endpoints). The
    # permissions endpoint re-validates the proxy -> principal binding against
    # the database on every request, so reassignment revokes permissions access
    # immediately regardless of exp. Rotation is
    # therefore infrequent: it exists to bound the lifetime of a leaked token,
    # not to enforce freshness. Keep the window long — every rotation changes
    # the synced config hash and forces a full config re-push to the proxy.
    # The TTL must comfortably exceed the window: iat is floored to the window
    # start, so a token delivered late in a window carries TTL - WINDOW of
    # remaining validity, which must cover proxy sync stalls.
    DEFAULT_WINDOW_SECONDS = 1.day.to_i
    DEFAULT_TTL_SECONDS = 3.days.to_i

    module_function

    def encode_for_proxy(proxy, now: Time.current)
      return nil unless proxy.assigned?

      CentaurJwt::WindowedToken.encode(
        subject_oid: proxy.oid,
        audience: audience,
        issuer: issuer,
        window_seconds: DEFAULT_WINDOW_SECONDS,
        ttl_seconds: DEFAULT_TTL_SECONDS,
        now: now,
        claims: {
          "sub" => proxy.name,
          "sandbox_id" => proxy.name,
          "proxy_id" => proxy.oid,
          "principal_id" => proxy.principal&.oid
        }
      )
    end

    def decode(token)
      CentaurJwt::Hs256.decode(
        token,
        signing_secret: ENV["CENTAUR_JWT_SIGNING_SECRET"].to_s,
        aud: audience,
        iss: issuer
      )
    end

    def audience
      ENV["CENTAUR_SANDBOX_ENTITLEMENTS_JWT_AUDIENCE"].presence || DEFAULT_AUDIENCE
    end

    def issuer
      ENV["CENTAUR_SANDBOX_ENTITLEMENTS_JWT_ISSUER"].presence || DEFAULT_ISSUER
    end
  end
end
