module Oauth
  module Providers
    # Shared failure handling for provider strategies that must call an account
    # endpoint after exchanging the authorization code. Identity is resolved
    # before any credential is persisted, so a failed lookup leaves no pending
    # credential or wrapper behind.
    module HttpIdentity
      private

      def identity_response(provider:)
        yield
      rescue Broker::ExchangeError
        raise
      rescue StandardError => e
        raise Broker::ExchangeError.new(
          "#{provider} identity endpoint request failed: #{e.class}",
          stage: "network",
          code: "identity_lookup_failed"
        )
      end

      def identity_json(response, provider:)
        unless response.success?
          raise Broker::ExchangeError.new(
            "#{provider} identity endpoint http #{response.status}",
            stage: "http",
            code: "identity_lookup_failed",
            status: response.status
          )
        end

        payload = response.json
        return payload if payload.is_a?(Hash)

        raise Broker::ExchangeError.new(
          "#{provider} identity endpoint returned an invalid payload",
          stage: "parse",
          code: "invalid_identity_response",
          status: response.status
        )
      rescue Broker::ExchangeError
        raise
      rescue JSON::ParserError, TypeError
        raise Broker::ExchangeError.new(
          "#{provider} identity endpoint returned invalid JSON",
          stage: "parse",
          code: "invalid_identity_response",
          status: response.status
        )
      end

      def require_identity(value, provider:)
        return value if value.present?

        raise Broker::ExchangeError.new(
          "#{provider} identity endpoint returned no stable subject",
          stage: "parse",
          code: "missing_identity"
        )
      end
    end
  end
end
