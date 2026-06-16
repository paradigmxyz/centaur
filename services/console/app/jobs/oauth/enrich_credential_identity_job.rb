module Oauth
  class EnrichCredentialIdentityJob < ApplicationJob
    queue_as :default

    def perform(credential_id)
      credential = BrokerCredential.includes(:oauth_app, :static_secret).find_by(id: credential_id)
      return unless credential&.oauth_app&.provider == Oauth::Providers::Slack::KEY
      return if credential.access_token.blank? || credential.provider_subject.blank?

      profile = credential.oauth_app.provider_strategy.slack_profile(
        credential.access_token,
        credential.provider_subject,
        credential.scopes.join(",")
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
  end
end
