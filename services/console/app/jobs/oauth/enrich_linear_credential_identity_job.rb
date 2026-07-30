require "json"

module Oauth
  class EnrichLinearCredentialIdentityJob < ApplicationJob
    queue_as :default

    GRAPHQL_ENDPOINT = Oauth::Providers::Linear::GRAPHQL_ENDPOINT
    VIEWER_QUERY = "{ viewer { id name email } }".freeze
    class LinearProfileRetryableError < StandardError; end

    retry_on LinearProfileRetryableError, wait: :polynomially_longer, attempts: 5 do |job, error|
      credential_id = job.arguments.first
      Rails.logger.warn do
        "linear oauth credential identity enrichment failed after retries: " \
          "credential_id=#{credential_id.inspect} error=#{error.class}"
      end
    end

    class << self
      attr_accessor :linear_api_http
    end

    def perform(credential_id)
      credential = BrokerCredential.includes(:oauth_app, :static_secret).find_by(id: credential_id)
      return unless credential&.oauth_app&.provider == Oauth::Providers::Linear::KEY
      return if credential.access_token.blank?

      profile = linear_profile(credential.access_token)
      subject = profile[:subject].presence
      display_name = profile[:name].presence || profile[:email].presence || subject
      if subject.blank? || display_name.blank?
        Rails.logger.warn do
          "linear oauth credential identity enrichment returned no identity: " \
            "credential=#{credential.oid}"
        end
        return
      end

      old_name = credential.name
      credential.update!(
        name: "Linear – #{display_name}",
        provider_subject: subject,
        provider_email: profile[:email].presence || credential.provider_email,
        foreign_id: "linear-#{credential.oauth_app.slug}-#{subject.downcase}"
      )

      secret = credential.static_secret
      return unless secret
      return if old_name.present? && secret.name != "#{old_name} token"

      secret.update!(name: "#{credential.name} token")
    rescue ActiveRecord::RecordInvalid, ActiveRecord::RecordNotUnique => e
      Rails.logger.warn do
        "linear oauth credential identity enrichment failed to persist: " \
          "credential=#{credential&.oid || credential_id.inspect} error=#{e.class}"
      end
    end

    private

    def linear_profile(access_token)
      response = linear_api(access_token)
      viewer = response.is_a?(Hash) ? response.dig("data", "viewer") : nil
      return {} unless viewer.is_a?(Hash)

      id = viewer["id"].presence
      return {} if id.blank?

      {
        subject: id.to_s,
        email: viewer["email"].presence,
        name: viewer["name"].presence
      }
    rescue LinearProfileRetryableError
      raise
    rescue StandardError => e
      Rails.logger.debug { "linear oauth profile lookup failed: #{e.class}" }
      {}
    end

    def linear_api(access_token)
      return nil if access_token.blank?

      if self.class.linear_api_http
        return self.class.linear_api_http.call(
          url: GRAPHQL_ENDPOINT,
          access_token: access_token,
          body: { query: VIEWER_QUERY }
        )
      end

      response = HttpClient.new.post(
        GRAPHQL_ENDPOINT,
        json: { query: VIEWER_QUERY },
        headers: {
          "Authorization" => "Bearer #{access_token}",
          "User-Agent" => "centaur-console"
        }
      )
      status = response.status
      if status == 429 || status >= 500
        raise LinearProfileRetryableError, "linear viewer lookup http #{status}"
      end
      unless status / 100 == 2
        Rails.logger.warn { "linear oauth profile lookup failed: status=#{status}" }
        return nil
      end

      parsed = response.json
      parsed.is_a?(Hash) ? parsed : nil
    rescue LinearProfileRetryableError
      raise
    rescue JSON::ParserError => e
      Rails.logger.warn { "linear oauth profile lookup returned invalid JSON: #{e.class}" }
      nil
    rescue StandardError => e
      raise LinearProfileRetryableError, e.class.name
    end
  end
end
