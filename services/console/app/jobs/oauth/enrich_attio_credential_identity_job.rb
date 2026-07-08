require "json"
require "net/http"
require "uri"

module Oauth
  class EnrichAttioCredentialIdentityJob < ApplicationJob
    queue_as :default

    SELF_ENDPOINT = "https://api.attio.com/v2/self"
    class AttioSelfRetryableError < StandardError; end

    retry_on AttioSelfRetryableError, wait: :polynomially_longer, attempts: 5 do |job, error|
      credential_id = job.arguments.first
      Rails.logger.warn do
        "attio oauth credential identity enrichment failed after retries: " \
          "credential_id=#{credential_id.inspect} error=#{error.class}"
      end
    end

    class << self
      attr_accessor :attio_api_http
    end

    def perform(credential_id)
      credential = BrokerCredential.includes(:oauth_app, :static_secret).find_by(id: credential_id)
      return unless credential&.oauth_app&.provider == Oauth::Providers::Attio::KEY
      return if credential.access_token.blank?

      workspace = attio_workspace(credential.access_token)
      subject = workspace[:subject].presence
      display_name = workspace[:name].presence || subject
      if subject.blank? || display_name.blank?
        Rails.logger.warn do
          "attio oauth credential identity enrichment returned no identity: " \
            "credential=#{credential.oid}"
        end
        return
      end

      old_name = credential.name
      credential.update!(
        name: "Attio – #{display_name}",
        provider_subject: subject,
        # Attio's token introspection identity is workspace-level, not user-level.
        provider_email: nil,
        foreign_id: "attio-#{credential.oauth_app.slug}-#{subject.downcase}"
      )

      secret = credential.static_secret
      return unless secret
      return if old_name.present? && secret.name != "#{old_name} token"

      secret.update!(name: "#{credential.name} token")
    rescue ActiveRecord::RecordInvalid, ActiveRecord::RecordNotUnique => e
      Rails.logger.warn do
        "attio oauth credential identity enrichment failed to persist: " \
          "credential=#{credential&.oid || credential_id.inspect} error=#{e.class}"
      end
    end

    private

    def attio_workspace(access_token)
      response = attio_api(access_token)
      return {} unless response.is_a?(Hash)

      workspace_id = response["workspace_id"].presence
      workspace_name = response["workspace_name"].presence
      return {} if workspace_id.blank?

      {
        subject: workspace_id.to_s,
        name: workspace_name || response["workspace_slug"].presence || workspace_id.to_s
      }
    rescue AttioSelfRetryableError
      raise
    rescue StandardError => e
      Rails.logger.debug { "attio oauth self lookup failed: #{e.class}" }
      {}
    end

    def attio_api(access_token)
      return nil if access_token.blank?

      if self.class.attio_api_http
        return self.class.attio_api_http.call(
          url: SELF_ENDPOINT,
          access_token: access_token
        )
      end

      uri = URI.parse(SELF_ENDPOINT)
      req = Net::HTTP::Get.new(uri)
      req["Accept"] = "application/json"
      req["Authorization"] = "Bearer #{access_token}"
      req["User-Agent"] = "centaur-console"

      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.open_timeout = 5
      http.read_timeout = 5

      response = http.request(req)
      status = response.code.to_i
      if status == 429 || status >= 500
        raise AttioSelfRetryableError, "attio self lookup http #{status}"
      end
      unless status / 100 == 2
        Rails.logger.warn { "attio oauth self lookup failed: status=#{status}" }
        return nil
      end

      parsed = JSON.parse(response.body.to_s)
      parsed.is_a?(Hash) ? parsed : nil
    rescue AttioSelfRetryableError
      raise
    rescue JSON::ParserError => e
      Rails.logger.warn { "attio oauth self lookup returned invalid JSON: #{e.class}" }
      nil
    rescue StandardError => e
      raise AttioSelfRetryableError, e.class.name
    end
  end
end
