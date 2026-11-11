# Operator console: a lightweight, server-rendered HTML view over credentials
# and OAuth apps (principals and secrets moved to the SPA). Read-only; gated
# behind a console session (ApplicationController#require_login) and restricted
# to admins (require_admin), like every Control page. Distinct from the JSON API.
class ConsoleController < ApplicationController
  layout "console"

  before_action :require_admin

  # Managed broker credentials and their refresh-loop status. Distinct from
  # SecretKinds::SECRET_KINDS because a broker credential is not grantable -- it is referenced
  # by a token_broker source rather than granted directly.
  def credentials
    @credentials = BrokerCredential.includes(:oauth_app).order(created_at: :asc, id: :asc)
  end

  def credential
    @credential = BrokerCredential.find_by_oid!(params[:id])
    # The grantable static secret wrapping this credential, for the cross-link.
    @wrapping_secret = @credential.static_secret
  end

  # Registered OAuth apps and the consent flows they drive. Like credentials,
  # an app is not grantable -- it is the durable config behind the public
  # /oauth/:provider/start flow.
  def oauth_apps
    @oauth_apps = OauthApp.order(created_at: :asc, id: :asc)
    # One count query for the whole table rather than one per row.
    @minted_counts = BrokerCredential.group(:oauth_app_id).count
  end

  def oauth_app
    @oauth_app = OauthApp.find_by_oid!(params[:id])
    @minted_credentials = @oauth_app.broker_credentials.order(created_at: :asc, id: :asc)
    @provider_details = {}
    @provider_credential = @minted_credentials.first if @oauth_app.shared?
    provider = @oauth_app.provider_strategy
    if provider.respond_to?(:details_for) && @provider_credential && !@provider_credential.dead? &&
        @provider_credential.access_token.present? && (@provider_credential.expires_at.nil? || @provider_credential.expires_at.future?)
      begin
        @provider_details = provider.details_for(@provider_credential,
          http_client: HttpClient.new(open_timeout: 2, read_timeout: 2, write_timeout: 2, max_body_bytes: 128 * 1024))
        @provider_details = {} unless @provider_details.is_a?(Hash)
      rescue StandardError
        # Optional provider details must never prevent credential management.
        @provider_details = {}
      end
    end
  end
end
