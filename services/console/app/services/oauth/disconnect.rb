module Oauth
  # Disconnects a personal connection. Local access is removed first, under the
  # credential's row lock (the lock token-broker and reconnect take), so no
  # refresh or proxy sync can use the tokens afterwards. When the provider has
  # an RFC 7009 revocation endpoint, the tokens are then revoked outside the
  # transaction so a slow endpoint never holds the lock.
  #
  # Returns :revoked, :local_only (no revocation endpoint), or :failed.
  class Disconnect
    DEAD_REASON = "disconnected".freeze

    def initialize(http: HttpClient.new(open_timeout: 5, read_timeout: 5))
      @http = http
    end

    def call(credential)
      tokens = nil
      credential.with_lock do
        tokens = [ [ credential.refresh_token, "refresh_token" ], [ credential.access_token, "access_token" ] ]
        credential.update!(access_token: nil, refresh_token: nil, expires_at: Time.current,
          dead: true, dead_reason: DEAD_REASON, next_attempt_at: nil)
      end
      provider = credential.oauth_app.provider_strategy
      endpoint = provider.revocation_endpoint if provider.respond_to?(:revocation_endpoint)
      return :local_only if endpoint.blank?

      revoke(credential, endpoint, tokens) ? :revoked : :failed
    end

    private

    def revoke(credential, endpoint, tokens)
      tokens.reject { |token, _| token.blank? }.map do |token, hint|
        @http.post(endpoint, form: {
          "client_id" => credential.effective_client_id,
          "client_secret" => credential.effective_client_secret,
          "token" => token,
          "token_type_hint" => hint
        }).success?
      end.all?
    rescue StandardError => e
      Rails.logger.warn { "oauth_revocation_failed credential=#{credential.oid} error=#{e.class}" }
      false
    end
  end
end
