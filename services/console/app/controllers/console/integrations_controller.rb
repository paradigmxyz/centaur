# The user-facing Integrations page: every enabled OauthApp with its public
# consent start link (/oauth/<slug>/start), so any signed-in team member can
# connect an integration without an operator sharing the link by hand.
#
# Deliberately not admin-gated (unlike ConsoleController): the whole point of
# the well-known consent links is that regular team members click them. Only
# non-sensitive fields are shown -- slug, provider, description -- never the
# client id/secret or minted credentials.
class Console::IntegrationsController < ApplicationController
  layout "console"

  def index
    @oauth_apps = OauthApp.where(enabled: true).order(:slug)
    @oauth_apps = @oauth_apps.where(shared: false) unless acting_admin?
    @presets = if acting_admin?
      configured = OauthApp.distinct.pluck(:provider)
      Oauth::Providers.registry.values.select do |provider|
        provider.respond_to?(:preset) && provider.respond_to?(:registration_endpoint) && !configured.include?(provider.key)
      end
    else
      []
    end
    # The user's existing connections: credentials they minted while signed in
    # (created_by, recorded by the consent callback) plus any whose IdP-reported
    # email matches their console login -- the fallback for consents made
    # without a console session. Newest wins if several match one app.
    mine = BrokerCredential.where(created_by: current_user)
      .or(BrokerCredential.where(provider_email: current_user.email))
    mine = mine.or(BrokerCredential.where(oauth_app_id: @oauth_apps.where(shared: true).select(:id)))
    @credentials_by_app_id = mine
      .where(oauth_app_id: @oauth_apps.select(:id))
      .order(:updated_at)
      .index_by(&:oauth_app_id)
  end
end
