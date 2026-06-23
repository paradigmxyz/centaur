require "json"
require "net/http"
require "uri"

module Oauth
  class EnrichGithubCredentialIdentityJob < ApplicationJob
    queue_as :default

    USER_ENDPOINT = "https://api.github.com/user"
    class GithubProfileRetryableError < StandardError; end

    retry_on GithubProfileRetryableError, wait: :polynomially_longer, attempts: 5 do |job, error|
      credential_id = job.arguments.first
      Rails.logger.warn do
        "github oauth credential identity enrichment failed after retries: " \
          "credential_id=#{credential_id.inspect} error=#{error.class}"
      end
    end

    class << self
      attr_accessor :github_api_http
    end

    def perform(credential_id)
      credential = BrokerCredential.includes(:oauth_app, :static_secret).find_by(id: credential_id)
      return unless credential&.oauth_app&.provider == Oauth::Providers::Github::KEY
      return if credential.access_token.blank?

      profile = github_profile(credential.access_token)
      subject = profile[:subject].presence
      display_name = profile[:name].presence || profile[:email].presence || subject
      if subject.blank? || display_name.blank?
        Rails.logger.warn do
          "github oauth credential identity enrichment returned no identity: " \
            "credential=#{credential.oid}"
        end
        return
      end

      old_name = credential.name
      credential.update!(
        name: "GitHub – #{display_name}",
        provider_subject: subject,
        provider_email: profile[:email].presence || credential.provider_email,
        foreign_id: "github-#{credential.oauth_app.slug}-#{subject.downcase}"
      )

      secret = credential.static_secret
      return unless secret
      return if old_name.present? && secret.name != "#{old_name} token"

      secret.update!(name: "#{credential.name} token")
    rescue ActiveRecord::RecordInvalid, ActiveRecord::RecordNotUnique => e
      Rails.logger.warn do
        "github oauth credential identity enrichment failed to persist: " \
          "credential=#{credential&.oid || credential_id.inspect} error=#{e.class}"
      end
    end

    private

    def github_profile(access_token)
      response = github_api(access_token)
      return {} unless response.is_a?(Hash)

      login = response["login"].presence
      id = response["id"]
      return {} if login.blank? || id.blank?

      {
        subject: id.to_s,
        email: response["email"].presence,
        name: response["name"].presence || login
      }
    rescue GithubProfileRetryableError
      raise
    rescue StandardError => e
      Rails.logger.debug { "github oauth profile lookup failed: #{e.class}" }
      {}
    end

    def github_api(access_token)
      return nil if access_token.blank?

      if self.class.github_api_http
        return self.class.github_api_http.call(
          url: USER_ENDPOINT,
          access_token: access_token
        )
      end

      uri = URI.parse(USER_ENDPOINT)
      req = Net::HTTP::Get.new(uri)
      req["Accept"] = "application/vnd.github+json"
      req["Authorization"] = "Bearer #{access_token}"
      req["X-GitHub-Api-Version"] = "2022-11-28"
      req["User-Agent"] = "centaur-console"

      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.open_timeout = 5
      http.read_timeout = 5

      response = http.request(req)
      status = response.code.to_i
      if status == 429 || status >= 500
        raise GithubProfileRetryableError, "github user lookup http #{status}"
      end
      unless status / 100 == 2
        Rails.logger.warn { "github oauth profile lookup failed: status=#{status}" }
        return nil
      end

      parsed = JSON.parse(response.body.to_s)
      parsed.is_a?(Hash) ? parsed : nil
    rescue GithubProfileRetryableError
      raise
    rescue JSON::ParserError => e
      Rails.logger.warn { "github oauth profile lookup returned invalid JSON: #{e.class}" }
      nil
    rescue StandardError => e
      raise GithubProfileRetryableError, e.class.name
    end
  end
end
