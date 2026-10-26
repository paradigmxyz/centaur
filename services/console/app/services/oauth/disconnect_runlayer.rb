module Oauth
  # The row lock serializes disconnect with rotating refresh-token jobs. Local
  # access is removed even if the remote revocation endpoint is unavailable.
  class DisconnectRunlayer
    def initialize(http: HttpClient.new(open_timeout: 5, read_timeout: 5))
      @http = http
    end

    def call(credential)
      revoked = true
      credential.with_lock do
        begin
          provider = credential.oauth_app.provider_strategy
          # A changed tenant configuration must never receive the old token.
          provider.refresh_resource(credential)
          [ [ credential.refresh_token, "refresh_token" ], [ credential.access_token, "access_token" ] ].each do |token, hint|
            next if token.blank?

            response = @http.post(provider.revocation_endpoint, form: {
              "client_id" => credential.effective_client_id,
              "client_secret" => credential.effective_client_secret,
              "token" => token,
              "token_type_hint" => hint
            })
            revoked = response.success? && revoked
          end
        rescue StandardError => e
          revoked = false
          Rails.logger.warn { "runlayer_revocation_failed credential=#{credential.oid} error=#{e.class}" }
        ensure
          credential.update!(access_token: nil, refresh_token: nil, expires_at: Time.current,
            dead: true, dead_reason: "disconnected", next_attempt_at: nil)
        end
      end
      revoked
    end
  end
end
