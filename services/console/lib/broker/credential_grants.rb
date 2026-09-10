module Broker
  # Registry for broker credential token-exchange strategies. BrokerCredential
  # owns persistence and scheduling; these strategies own provider-specific
  # request shapes and bootstrap validation.
  module CredentialGrants
    require "base64"
    require "json"
    require "net/http"
    require "openssl"
    require "time"
    require "uri"

    PREQIN_TOKEN_ENDPOINT = "https://api.preqin.com/connect/token".freeze
    PREQIN_REFRESH_TOKEN_ENDPOINT = "https://api.preqin.com/connect/refresh_token".freeze

    GRANTS = %w[refresh_token client_credentials password preqin github_app].freeze
    REFRESHABLE_WITHOUT_TOKEN_GRANTS = %w[client_credentials password preqin github_app].freeze

    Outcome = Data.define(:result, :clear_refresh_token, :dead_reason)

    class << self
      attr_writer :github_app_http_client

      def default_token_endpoint(grant)
        PREQIN_TOKEN_ENDPOINT if grant == "preqin"
      end

      def client_id_required?(credential)
        credential.grant != "preqin" && !credential.oauth_app_id?
      end

      def validate(credential)
        case credential.grant
        when "client_credentials"
          validate_client_credentials(credential)
        when "password"
          validate_password(credential)
        when "preqin"
          validate_preqin(credential)
        when "github_app"
          validate_github_app(credential)
        end
      end

      def refresh(credential)
        case credential.grant
        when "client_credentials"
          refresh_client_credentials(credential)
        when "password"
          refresh_password(credential)
        when "preqin"
          refresh_preqin(credential)
        when "github_app"
          refresh_github_app(credential)
        else
          refresh_token(credential)
        end
      end

      private

      def success(result, clear_refresh_token: false)
        Outcome.new(result: result, clear_refresh_token: clear_refresh_token, dead_reason: nil)
      end

      def dead(reason)
        Outcome.new(result: nil, clear_refresh_token: false, dead_reason: reason)
      end

      def refresh_token(credential)
        return dead("missing_initial_refresh_token") if credential.refresh_token.blank?

        success(oauth_refresh_token(credential))
      end

      def refresh_password(credential)
        clear_stale_refresh_token = false

        if credential.refresh_token.present?
          begin
            return success(oauth_refresh_token(credential))
          rescue Broker::RefreshError => e
            raise if e.retryable?

            Rails.logger.warn do
              "broker credential #{credential.oid} refresh_token grant failed with #{e.reason}; " \
                "falling back to password grant"
            end
            clear_stale_refresh_token = true
          end
        end

        return dead("password_grant_missing_initial_values") unless password_values_present?(credential)

        result = post_token_form(credential, url: credential.token_endpoint,
                                             form: password_form(credential))
        success(result, clear_refresh_token: clear_stale_refresh_token && result.refresh_token.blank?)
      end

      def refresh_client_credentials(credential)
        result = post_token_form(
          credential,
          url: credential.token_endpoint,
          form: client_credentials_form(credential)
        )
        success(result)
      end

      def refresh_preqin(credential)
        clear_stale_refresh_token = false

        if credential.refresh_token.present?
          begin
            result = post_token_form(
              credential,
              url: PREQIN_REFRESH_TOKEN_ENDPOINT,
              form: preqin_refresh_token_form(credential),
              form_encoding: :multipart,
              strict_4xx: true
            )
            return success(result)
          rescue Broker::RefreshError => e
            raise if e.retryable?

            Rails.logger.warn do
              "broker credential #{credential.oid} preqin refresh_token failed with #{e.reason}; " \
                "falling back to username/api key"
            end
            clear_stale_refresh_token = true
          end
        end

        return dead("preqin_missing_initial_values") unless preqin_values_present?(credential)

        result = post_token_form(
          credential,
          url: credential.token_endpoint,
          form: preqin_token_form(credential),
          form_encoding: :multipart,
          strict_4xx: true
        )
        success(result, clear_refresh_token: clear_stale_refresh_token && result.refresh_token.blank?)
      end

      def refresh_github_app(credential)
        now = Time.current
        require_value!("client_id", credential.client_id)
        require_value!("client_secret", credential.client_secret)
        require_value!("token_endpoint", credential.token_endpoint)

        uri = URI.parse(credential.token_endpoint)
        request = Net::HTTP::Post.new(uri)
        request["Authorization"] = "Bearer #{github_app_jwt(credential, now)}"
        request["Accept"] = "application/vnd.github+json"
        request["X-GitHub-Api-Version"] = "2022-11-28"

        response = github_app_http_request(uri, request, credential.refresh_timeout_seconds)
        unless response.is_a?(Net::HTTPSuccess)
          raise github_app_http_error(response)
        end

        parsed = JSON.parse(response.body)
        token = parsed["token"].to_s
        raise Broker::RefreshError.new("GitHub App token response missing token", stage: "parse", retryable: true) if token.blank?

        expires_at = Time.iso8601(parsed.fetch("expires_at"))
        expires_in = [ (expires_at - now).to_i, 1 ].max
        result = Broker::RefreshClient::Result.new(
          access_token: token,
          refresh_token: nil,
          expires_in: expires_in
        )
        success(result, clear_refresh_token: true)
      rescue JSON::ParserError, KeyError, ArgumentError, TypeError => e
        raise Broker::RefreshError.new(
          "GitHub App token response could not be parsed: #{e.class}",
          stage: "parse",
          retryable: true
        )
      rescue OpenSSL::PKey::PKeyError => e
        raise Broker::RefreshError.new(
          "GitHub App private key is invalid: #{e.class}",
          stage: "config",
          code: "invalid_private_key",
          retryable: false
        )
      rescue URI::InvalidURIError => e
        raise Broker::RefreshError.new(
          "GitHub App token endpoint is invalid: #{e.class}",
          stage: "config",
          code: "invalid_token_endpoint",
          retryable: false
        )
      rescue IOError, SystemCallError, Timeout::Error, Net::OpenTimeout, Net::ReadTimeout => e
        raise Broker::RefreshError.new(
          "GitHub App token endpoint request failed: #{e.class}",
          stage: "network",
          retryable: true
        )
      end

      def oauth_refresh_token(credential)
        post_token_form(
          credential,
          url: credential.token_endpoint,
          form: refresh_token_form(credential)
        )
      end

      def post_token_form(credential, url:, form:, form_encoding: :urlencoded, strict_4xx: false)
        credential.refresh_client.refresh(
          url: url,
          form: form,
          form_encoding: form_encoding,
          headers: credential.token_endpoint_headers || {},
          timeout: credential.refresh_timeout_seconds,
          strict_4xx: strict_4xx
        )
      end

      def refresh_token_form(credential)
        require_value!("client_id", credential.effective_client_id)
        require_value!("refresh_token", credential.refresh_token)

        form = {
          "grant_type" => "refresh_token",
          "refresh_token" => credential.refresh_token,
          "client_id" => credential.effective_client_id
        }
        add_oauth_optional_fields(form, credential)
      end

      def password_form(credential)
        require_value!("client_id", credential.effective_client_id)
        require_value!("username", credential.username)
        require_value!("password", credential.password)

        form = {
          "grant_type" => "password",
          "username" => credential.username,
          "password" => credential.password,
          "client_id" => credential.effective_client_id
        }
        add_oauth_optional_fields(form, credential)
      end

      def client_credentials_form(credential)
        require_value!("client_id", credential.effective_client_id)
        require_value!("client_secret", credential.effective_client_secret)

        form = {
          "grant_type" => "client_credentials",
          "client_id" => credential.effective_client_id,
          "client_secret" => credential.effective_client_secret
        }
        add_oauth_optional_fields(form, credential)
      end

      def preqin_token_form(credential)
        require_value!("username", credential.username)
        require_value!("api_key", credential.api_key)

        {
          "username" => credential.username,
          "apikey" => credential.api_key
        }
      end

      def preqin_refresh_token_form(credential)
        require_value!("refresh_token", credential.refresh_token)

        { "refresh_token" => credential.refresh_token }
      end

      def github_app_jwt(credential, now)
        header = base64url(JSON.generate({ alg: "RS256", typ: "JWT" }))
        payload = base64url(JSON.generate({
          iat: now.to_i - 60,
          exp: now.to_i + 9.minutes.to_i,
          iss: credential.client_id
        }))
        signing_input = "#{header}.#{payload}"
        key = OpenSSL::PKey::RSA.new(github_app_private_key_pem(credential.client_secret))
        signature = key.sign(OpenSSL::Digest.new("SHA256"), signing_input)
        "#{signing_input}.#{base64url(signature)}"
      end

      def github_app_private_key_pem(value)
        text = value.to_s
        return text if text.include?("-----BEGIN")

        Base64.strict_decode64(text)
      rescue ArgumentError
        text
      end

      def github_app_http_request(uri, request, timeout)
        if @github_app_http_client
          return @github_app_http_client.call(uri, request)
        end

        Net::HTTP.start(uri.host, uri.port, use_ssl: uri.scheme == "https",
                        open_timeout: timeout, read_timeout: timeout) do |http|
          http.request(request)
        end
      end

      def github_app_http_error(response)
        status = response.code.to_i
        retryable = status / 100 == 5 || status == 429
        Broker::RefreshError.new(
          "GitHub App installation token endpoint returned HTTP #{status}",
          stage: "http",
          code: "http_#{status}",
          status: status,
          retryable: retryable
        )
      end

      def base64url(value)
        Base64.urlsafe_encode64(value, padding: false)
      end

      def add_oauth_optional_fields(form, credential)
        form["client_secret"] = credential.effective_client_secret if credential.effective_client_secret.present?

        scopes = credential.refresh_scopes_for_provider
        form["scope"] = scopes.join(" ") if scopes.present?
        form
      end

      def require_value!(name, value)
        raise ArgumentError, "#{name} is required" if value.blank?
      end

      def validate_password(credential)
        credential.errors.add(:username, "can't be blank for the password grant") if credential.username.blank?
        credential.errors.add(:password, "can't be blank for the password grant") if credential.password.blank?
      end

      def validate_client_credentials(credential)
        if credential.effective_client_secret.blank?
          credential.errors.add(:client_secret, "can't be blank for the client_credentials grant")
        end
      end

      def validate_preqin(credential)
        credential.errors.add(:username, "can't be blank for the Preqin broker grant") if credential.username.blank?
        credential.errors.add(:api_key, "can't be blank for the Preqin broker grant") if credential.api_key.blank?
      end

      def validate_github_app(credential)
        if credential.client_secret.blank?
          credential.errors.add(:client_secret, "can't be blank for the GitHub App broker grant")
        end
      end

      def password_values_present?(credential)
        credential.username.present? && credential.password.present?
      end

      def preqin_values_present?(credential)
        credential.username.present? && credential.api_key.present?
      end
    end
  end
end
