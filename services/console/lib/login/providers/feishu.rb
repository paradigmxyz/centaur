require "json"

module Login
  module Providers
    # China Feishu OAuth uses a JSON token request and a separate user-info call,
    # so this strategy owns both calls and returns only the durable identity.
    class Feishu
      KEY = "feishu".freeze
      AUTHORIZATION_ENDPOINT = "https://accounts.feishu.cn/open-apis/authen/v1/authorize".freeze
      TOKEN_ENDPOINT = "https://accounts.feishu.cn/oauth/v3/token".freeze
      USER_INFO_ENDPOINT = "https://open.feishu.cn/open-apis/authen/v1/user_info".freeze
      SCOPES = %w[contact:user.employee:readonly].freeze
      MAX_BODY_BYTES = 64 * 1024
      HTTP_TIMEOUT = 30

      def key = KEY
      def authorization_endpoint = AUTHORIZATION_ENDPOINT
      def token_endpoint = TOKEN_ENDPOINT
      def user_info_endpoint = USER_INFO_ENDPOINT
      def scopes = SCOPES
      def extra_authorization_params = {}
      def pkce? = true

      def self.subject(tenant_key:, union_id:)
        JSON.generate([ tenant_key.to_s, union_id.to_s ])
      end

      def exchange_identity(code:, client_id:, client_secret:, redirect_uri:, code_verifier:,
                            allowed_tenant_keys:, http: default_http)
        token = exchange_token(code:, client_id:, client_secret:, redirect_uri:, code_verifier:, http:)
        identity_from_user_info(fetch_user_info(token, http:), allowed_tenant_keys:)
      end

      private

      def default_http
        HttpClient.new(
          open_timeout: HTTP_TIMEOUT,
          read_timeout: HTTP_TIMEOUT,
          max_body_bytes: MAX_BODY_BYTES
        )
      end

      def exchange_token(code:, client_id:, client_secret:, redirect_uri:, code_verifier:, http:)
        response = http.post(
          TOKEN_ENDPOINT,
          json: {
            "grant_type" => "authorization_code",
            "client_id" => client_id,
            "client_secret" => client_secret,
            "code" => code,
            "redirect_uri" => redirect_uri,
            "code_verifier" => code_verifier
          }
        )
        payload = parse_json(response, stage: "token")
        unless response.success?
          raise exchange_error(
            "token endpoint rejected authorization code",
            "oauth",
            code: payload["error"].presence,
            status: response.status
          )
        end
        unless payload["code"].to_i.zero?
          raise exchange_error(
            "token endpoint rejected authorization code",
            "oauth",
            code: payload["error"].presence || "feishu_#{payload["code"]}",
            status: response.status
          )
        end
        payload["access_token"].to_s.strip.presence ||
          raise(exchange_error("token endpoint returned no access token", "parse", code: "missing_access_token"))
      rescue Broker::ExchangeError
        raise
      rescue StandardError => e
        raise exchange_error("token endpoint request failed: #{e.class}", "network")
      end

      def fetch_user_info(access_token, http:)
        response = http.get(
          USER_INFO_ENDPOINT,
          headers: { "Authorization" => "Bearer #{access_token}" }
        )
        payload = parse_json(response, stage: "userinfo")
        unless response.success?
          raise exchange_error("user-info request failed", "http", code: "userinfo_http", status: response.status)
        end
        unless payload["code"].to_i.zero? && payload["data"].is_a?(Hash)
          raise exchange_error(
            "user-info provider response was invalid",
            "oauth",
            code: "invalid_user_info",
            status: response.status
          )
        end
        payload.fetch("data")
      rescue Broker::ExchangeError
        raise
      rescue StandardError => e
        raise exchange_error("user-info request failed: #{e.class}", "network")
      end

      def identity_from_user_info(data, allowed_tenant_keys:)
        tenant_key = required_identity_field(data, "tenant_key")
        open_id = required_identity_field(data, "open_id")
        union_id = required_identity_field(data, "union_id")
        email = required_identity_field(data, "enterprise_email").downcase
        allowed = Array(allowed_tenant_keys).map { |key| key.to_s.strip }.reject(&:blank?)
        unless allowed.include?(tenant_key)
          raise exchange_error("Feishu tenant is not allowed", "oauth", code: "tenant_not_allowed")
        end

        {
          subject: self.class.subject(tenant_key:, union_id:),
          tenant_key:,
          open_id:,
          email:,
          email_verified: true,
          name: data["name"].to_s.strip.presence
        }
      end

      def required_identity_field(data, key)
        data[key].to_s.strip.presence ||
          raise(exchange_error("Feishu user-info is missing required identity fields", "parse",
                               code: "invalid_user_info"))
      end

      def parse_json(response, stage:)
        JSON.parse(response.body.to_s)
      rescue JSON::ParserError, TypeError
        raise exchange_error("#{stage} response was not valid JSON", "parse")
      end

      def exchange_error(message, stage, code: nil, status: nil)
        Broker::ExchangeError.new(message, stage:, code:, status:)
      end
    end
  end
end
