module Api
  module V1
    module Console
      # GET /api/v1/console/integrations
      #
      # The Integrations page for the caller: enabled OAuth apps with their
      # public consent start links and the caller's connection state, plus (for
      # admins) shared apps and the presets still to set up.
      #
      # DELETE /api/v1/console/integrations/connections/:id
      #
      # Disconnects one of the caller's own personal connections (see
      # Oauth::Disconnect).
      # Open to any active account. Only non-sensitive app fields are returned --
      # never the client id/secret or other users' credentials.
      class IntegrationsController < BaseController
        include OauthAppUrls

        allow_active_users only: %i[index disconnect]

        # Revokes tokens at the provider (tests swap it).
        class_attribute :disconnect_service_factory, default: -> { Oauth::Disconnect.new }

        USAGE = { shared: "shared", always_available: "always_available", direct_messages: "direct_messages" }.freeze

        def index
          integrations = ConsoleIntegrations.new(user: current_user, acting_admin: acting_admin?)
          render json: { data: {
            apps: integrations.apps.map { |app| app_payload(app, integrations.credential_for(app)) },
            presets: integrations.presets.map { |provider| preset_payload(provider) },
            ssh_key: { configured: integrations.ssh_key_configured? }
          } }
        end

        def disconnect
          credential = BrokerCredential.joins(:oauth_app)
            .where(created_by: current_user, oauth_apps: { shared: false })
            .find_by_oid!(params[:id])
          outcome = disconnect_service_factory.call.call(credential)
          render json: { data: { id: credential.oid, outcome: outcome.to_s } }
        end

        private

        def app_payload(app, credential)
          {
            id: app.oid,
            slug: app.slug,
            provider: app.provider,
            description: app.description,
            shared: app.shared,
            always_available: app.always_available,
            owner_only: app.owner_only,
            usage: usage(app),
            start_url: oauth_start_url(app.slug),
            connection: credential && {
              id: credential.oid,
              status: app.credential_needs_reconnect?(credential) ? "needs_reconnect" : "connected",
              # Only the user who made a personal connection may disconnect it.
              can_disconnect: !app.shared? && credential.created_by_id == current_user.id && !credential.dead?
            }
          }
        end

        def usage(app)
          return USAGE[:shared] if app.shared?

          app.always_available? ? USAGE[:always_available] : USAGE[:direct_messages]
        end

        def preset_payload(preset)
          { provider: preset.key, display_name: preset.display_name, description: preset.preset[:description],
            inputs: preset.respond_to?(:setup_inputs) ? preset.setup_inputs : [] }
        end
      end
    end
  end
end
