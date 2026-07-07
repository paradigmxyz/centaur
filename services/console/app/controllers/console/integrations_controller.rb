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
    # The user's existing connections, matched by the email the IdP reported
    # during consent (the flow itself is unauthenticated, so provider_email is
    # the only link back to a console user). Newest wins if the user somehow
    # consented with several provider accounts sharing the email.
    @credentials_by_app_id = BrokerCredential
      .where(oauth_app_id: @oauth_apps.select(:id), provider_email: current_user.email)
      .order(:updated_at)
      .index_by(&:oauth_app_id)
  end
end
