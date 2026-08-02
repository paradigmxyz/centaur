require "cgi"
require "json"

module Oauth
  class EnrichHubspotCredentialIdentityJob < ApplicationJob
    queue_as :default

    ACCESS_TOKEN_INFO_ENDPOINT = Oauth::Providers::Hubspot::ACCESS_TOKEN_INFO_ENDPOINT
    class HubspotProfileRetryableError < StandardError; end

    retry_on HubspotProfileRetryableError, wait: :polynomially_longer, attempts: 5 do |job, error|
      credential_id = job.arguments.first
      Rails.logger.warn do
        "hubspot oauth credential identity enrichment failed after retries: " \
          "credential_id=#{credential_id.inspect} error=#{error.class}"
      end
    end

    class << self
      attr_accessor :hubspot_api_http
    end

    def perform(credential_id)
      credential = BrokerCredential.includes(:oauth_app, :static_secret).find_by(id: credential_id)
      return unless credential&.oauth_app&.provider == Oauth::Providers::Hubspot::KEY
      return if credential.access_token.blank?

      profile = hubspot_profile(credential.access_token)
      subject = profile[:subject].presence
      display_name = profile[:name].presence || profile[:email].presence || subject
      if subject.blank? || display_name.blank?
        Rails.logger.warn do
          "hubspot oauth credential identity enrichment returned no identity: " \
            "credential=#{credential.oid}"
        end
        return
      end

      old_name = credential.name
      labels = (credential.labels || {}).merge(
        "hubspot_hub_id" => profile[:hub_id],
        "hubspot_user_id" => profile[:user_id]
      ).compact
      labels["hubspot_hub_domain"] = profile[:hub_domain] if profile[:hub_domain].present?

      credential.update!(
        name: "HubSpot – #{display_name}",
        provider_subject: subject,
        provider_email: profile[:email].presence || credential.provider_email,
        foreign_id: "hubspot-#{credential.oauth_app.slug}-#{subject}",
        labels: labels
      )

      secret = credential.static_secret
      return unless secret
      return if old_name.present? && secret.name != "#{old_name} token"

      secret.update!(name: "#{credential.name} token")
    rescue ActiveRecord::RecordInvalid, ActiveRecord::RecordNotUnique => e
      Rails.logger.warn do
        "hubspot oauth credential identity enrichment failed to persist: " \
          "credential=#{credential&.oid || credential_id.inspect} error=#{e.class}"
      end
    end

    private

    def hubspot_profile(access_token)
      response = hubspot_api(access_token)
      return {} unless response.is_a?(Hash)

      hub_id = response["hub_id"]
      user_id = response["user_id"]
      return {} if hub_id.blank? || user_id.blank?

      email = response["user"].presence
      {
        subject: "#{hub_id}:#{user_id}",
        email: email,
        name: email,
        hub_id: hub_id.to_s,
        user_id: user_id.to_s,
        hub_domain: response["hub_domain"].presence
      }
    rescue HubspotProfileRetryableError
      raise
    rescue StandardError => e
      Rails.logger.debug { "hubspot oauth profile lookup failed: #{e.class}" }
      {}
    end

    def hubspot_api(access_token)
      return nil if access_token.blank?

      url = "#{ACCESS_TOKEN_INFO_ENDPOINT}/#{CGI.escape(access_token)}"

      if self.class.hubspot_api_http
        return self.class.hubspot_api_http.call(
          url: ACCESS_TOKEN_INFO_ENDPOINT,
          access_token: access_token
        )
      end

      response = HttpClient.new.get(url, headers: { "Accept" => "application/json" })
      status = response.status
      if status == 429 || status >= 500
        raise HubspotProfileRetryableError, "hubspot token info lookup http #{status}"
      end
      unless status / 100 == 2
        Rails.logger.warn { "hubspot oauth profile lookup failed: status=#{status}" }
        return nil
      end

      parsed = response.json
      parsed.is_a?(Hash) ? parsed : nil
    rescue HubspotProfileRetryableError
      raise
    rescue JSON::ParserError => e
      Rails.logger.warn { "hubspot oauth profile lookup returned invalid JSON: #{e.class}" }
      nil
    rescue StandardError => e
      raise HubspotProfileRetryableError, e.class.name
    end
  end
end
