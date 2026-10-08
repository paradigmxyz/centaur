module Oauth
  # RFC 7591 registration for presets whose provider supplies an endpoint.
  class DynamicRegistration
    def initialize(http: HttpClient.new(max_body_bytes: 128 * 1024))
      @http = http
    end

    def register(endpoint:, redirect_uri:, scope:, auth_method:)
      response = @http.post(endpoint, json: {
        client_name: "Centaur", redirect_uris: [ redirect_uri ],
        grant_types: %w[authorization_code refresh_token], response_types: [ "code" ],
        token_endpoint_auth_method: auth_method, scope: scope
      })
      body = response.json if response.success?
      unless body.is_a?(Hash) && %w[client_id client_secret].all? { |key| body[key].is_a?(String) && body[key].present? }
        raise Broker::ExchangeError.new("Client registration failed", stage: "parse")
      end
      { client_id: body.fetch("client_id"), client_secret: body.fetch("client_secret") }
    rescue Broker::ExchangeError
      raise
    rescue StandardError
      raise Broker::ExchangeError.new("Client registration unavailable", stage: "network")
    end
  end
end
