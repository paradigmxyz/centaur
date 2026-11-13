# What the Integrations page shows one console user: every enabled OAuth app
# they may connect (shared apps only for admins), their newest
# connection to each, the presets an admin can still set up,
# and whether they saved a personal SSH key.
#
# A user's connections are the credentials they minted while signed in
# (created_by, recorded by the consent callback) plus any whose IdP-reported
# email matches their console login -- the fallback for consents made without
# a console session. Every credential of a shared app counts, because a shared
# app has one admin-connected account. Newest wins if several match one app.
# A connection the user disconnected no longer counts.
class ConsoleIntegrations
  def initialize(user:, acting_admin:)
    @user = user
    @acting_admin = acting_admin
  end

  def apps
    @apps ||= begin
      scope = OauthApp.where(enabled: true).order(:slug)
      scope = scope.where(shared: false) unless @acting_admin
      scope.to_a
    end
  end

  # The user's newest credential for +app+, or nil.
  def credential_for(app)
    credentials_by_app_id[app.id]
  end

  def presets
    @acting_admin ? OauthPresetSetup.available_presets : []
  end

  def ssh_key_configured?
    @user.personal_ssh_key.present?
  end

  private

  def credentials_by_app_id
    @credentials_by_app_id ||= begin
      app_ids = apps.map(&:id)
      shared_ids = apps.select(&:shared?).map(&:id)
      mine = BrokerCredential.where(created_by: @user).or(BrokerCredential.where(provider_email: @user.email))
      mine = mine.or(BrokerCredential.where(oauth_app_id: shared_ids)) if shared_ids.any?
      mine.where(oauth_app_id: app_ids).where("dead_reason IS DISTINCT FROM ?", Oauth::Disconnect::DEAD_REASON)
        .order(:updated_at).index_by(&:oauth_app_id)
    end
  end
end
