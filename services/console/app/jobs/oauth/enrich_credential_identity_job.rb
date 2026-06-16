require "json"
require "net/http"
require "uri"

module Oauth
  class EnrichCredentialIdentityJob < ApplicationJob
    queue_as :default

    AUTH_TEST_ENDPOINT = "https://slack.com/api/auth.test"
    USERS_INFO_ENDPOINT = "https://slack.com/api/users.info"

    class << self
      attr_accessor :slack_api_http
    end

    def perform(credential_id)
      credential = BrokerCredential.includes(:oauth_app, :static_secret).find_by(id: credential_id)
      return unless credential&.oauth_app&.provider == Oauth::Providers::Slack::KEY
      return if credential.access_token.blank? || credential.provider_subject.blank?

      profile = slack_profile(
        credential.access_token,
        credential.provider_subject,
        credential.scopes
      )
      display_name = profile[:name].presence || profile[:email].presence
      return if display_name.blank?

      new_name = "#{credential.oauth_app.provider.capitalize} – #{display_name}"
      old_name = credential.name
      credential.update!(
        name: new_name,
        provider_email: profile[:email].presence || credential.provider_email
      )

      secret = credential.static_secret
      return unless secret
      return if old_name.present? && secret.name != "#{old_name} token"

      secret.update!(name: "#{new_name} token")
    end

    private

    def slack_profile(access_token, user_id, scopes)
      profile = {}
      profile[:name] = auth_test_user(access_token)
      return profile unless scopes.include?("users:read")

      info = slack_api(USERS_INFO_ENDPOINT, access_token, "user" => user_id)
      return profile unless info.is_a?(Hash) && info["ok"] == true

      user = info["user"].is_a?(Hash) ? info["user"] : {}
      user_profile = user["profile"].is_a?(Hash) ? user["profile"] : {}
      profile[:name] = user_profile["display_name"].presence ||
                       user_profile["real_name"].presence ||
                       user["real_name"].presence ||
                       user["name"].presence ||
                       profile[:name]
      profile[:email] = user_profile["email"].presence if scopes.include?("users:read.email")
      profile
    rescue StandardError => e
      Rails.logger.debug { "slack oauth profile lookup failed: #{e.class}" }
      profile
    end

    def auth_test_user(access_token)
      response = slack_api(AUTH_TEST_ENDPOINT, access_token)
      return nil unless response.is_a?(Hash) && response["ok"] == true
      response["user"].presence
    end

    def slack_api(url, access_token, params = {})
      return nil if access_token.blank?

      if self.class.slack_api_http
        return self.class.slack_api_http.call(
          url: url, access_token: access_token, params: params
        )
      end

      uri = URI.parse(url)
      req = Net::HTTP::Post.new(uri)
      req["Authorization"] = "Bearer #{access_token}"
      req["Accept"] = "application/json"
      req.set_form_data(params) if params.any?

      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.open_timeout = 5
      http.read_timeout = 5

      JSON.parse(http.request(req).body.to_s)
    end
  end
end
