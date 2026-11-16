module Api
  module V1
    module Console
      # Broker credentials for the console SPA (session only). Payloads add the
      # presence (never the value) of each write-only field, the effective
      # client id, and the owning OAuth app and wrapping static secret. On
      # update, a `null` token_endpoint_headers value for an already stored
      # header name keeps that stored value, so the editor can keep or rename
      # headers without re-entering their write-only values.
      class BrokerCredentialsController < ::Api::V1::BrokerCredentialsController
        include ConsoleApiAuthentication

        def index
          records, meta = paginated_label_search(BrokerCredential.includes(:oauth_app, :static_secret))
          render json: { data: records.map { |r| record_payload(r) }, meta: meta }
        end

        private

        def apply_token_endpoint_headers(ref, attrs)
          stored = ref.token_endpoint_headers
          super
          headers = ref.token_endpoint_headers
          return unless attrs.key?(:token_endpoint_headers) && headers.is_a?(Hash) && stored.is_a?(Hash)

          ref.token_endpoint_headers =
            headers.to_h { |name, value| [ name, value.nil? && stored.key?(name) ? stored[name] : value ] }
        end

        def record_payload(ref)
          super.merge(
            has_client_secret: ref.client_secret.present?,
            has_refresh_token: ref.refresh_token.present?,
            has_username: ref.username.present?,
            has_password: ref.password.present?,
            has_api_key: ref.api_key.present?,
            # The client id the refresh loop uses (inherited from the OAuth app
            # for flow-minted credentials).
            effective_client_id: ref.effective_client_id,
            oauth_app: ref.oauth_app && { id: ref.oauth_app.oid, slug: ref.oauth_app.slug, provider: ref.oauth_app.provider },
            # The grantable static secret wrapping this credential, if any.
            static_secret: ref.static_secret && {
              id: ref.static_secret.oid, foreign_id: ref.static_secret.foreign_id, name: ref.static_secret.name
            }
          )
        end
      end
    end
  end
end
