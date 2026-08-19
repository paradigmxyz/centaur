module Broker
  # Registry for broker credential token-exchange strategies. BrokerCredential
  # owns persistence and scheduling; these strategies own provider-specific
  # request shapes and bootstrap validation.
  module CredentialGrants
    PREQIN_TOKEN_ENDPOINT = "https://api.preqin.com/connect/token".freeze
    PREQIN_REFRESH_TOKEN_ENDPOINT = "https://api.preqin.com/connect/refresh_token".freeze

    GRANTS = %w[refresh_token client_credentials password preqin].freeze
    REFRESHABLE_WITHOUT_TOKEN_GRANTS = %w[client_credentials password preqin].freeze

    Outcome = Data.define(:result, :clear_refresh_token, :dead_reason)

    class << self
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

      def password_values_present?(credential)
        credential.username.present? && credential.password.present?
      end

      def preqin_values_present?(credential)
        credential.username.present? && credential.api_key.present?
      end
    end
  end
end
