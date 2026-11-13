module Api
  module V1
    module Console
      # OAuth apps for the console SPA (session only). Payloads add
      # client_secret_set (presence only), redirect_uri, start_url, and
      # credentials_count; the console-only actions list minted credentials,
      # read live provider details, and set up provider presets.
      class OauthAppsController < ::Api::V1::OauthAppsController
        include ConsoleApiAuthentication
        include OauthAppUrls

        # Builds the RFC 7591 client used by #preset (tests swap it).
        class_attribute :registration_client_factory, default: -> { Oauth::DynamicRegistration.new }

        PRESET_CONFLICT_MESSAGE = "The preset conflicts with an existing OAuth app. Review its configuration.".freeze
        PRESET_FAILED_MESSAGE = "Could not set up the integration. Please try again or review the OAuth app configuration.".freeze
        PRESET_INPUT_MESSAGE = "Enter a valid %s MCP URL.".freeze

        def index
          records, meta = paginated_apps
          counts = BrokerCredential.where(oauth_app_id: records.map(&:id)).group(:oauth_app_id).count
          render json: { data: records.map { |r| record_payload(r, credentials_count: counts.fetch(r.id, 0)) }, meta: meta }
        end

        # GET /api/v1/console/oauth_apps/:id/credentials
        #
        # The broker credentials this app minted, oldest first. A summary only:
        # the token blob is never serialized.
        def credentials
          app = OauthApp.find_by_oid!(params[:id])
          records, meta = paginated_label_search(app.broker_credentials)
          render json: { data: records.map { |r| minted_credential_payload(r) }, meta: meta }
        end

        # GET /api/v1/console/oauth_apps/:id/provider_details
        #
        # Provider-specific details for a shared app's connected account (e.g. the
        # Mercator wallet's claim state). Fetched live with tight timeouts; any
        # failure yields empty details rather than an error.
        def provider_details
          details = OauthAppProviderDetails.new(OauthApp.find_by_oid!(params[:id]))
          credential = details.credential
          render json: { data: {
            provider: details.app.provider,
            credential: credential && {
              id: credential.oid,
              provider_subject: credential.provider_subject,
              static_secret_id: credential.static_secret&.oid
            },
            details: credential ? details.details : {}
          } }
        end

        # POST /api/v1/console/oauth_apps/presets/:provider
        #
        # Sets up a preset (registering an OAuth client with the provider when
        # the preset's app doesn't exist yet) and returns the consent start URL
        # to send the admin to. A preset with inputs (e.g. Runlayer's tenant MCP
        # URL) reads them from `data`.
        def preset
          source = Oauth::Presets.fetch(params[:provider])
          return render_error(status: :not_found, message: "no preset for this provider") unless source

          input = params.fetch(:data, {}).permit(:resource_url).to_h
          app = OauthPresetSetup.new(
            preset: source, user: current_user, input: input,
            redirect_uri_for: method(:oauth_callback_redirect_uri),
            registration_client: registration_client_factory.call
          ).call
          render json: { data: { id: app.oid, slug: app.slug, start_url: oauth_start_url(app.slug) } }
        rescue OauthPresetSetup::InvalidInput
          render_error(status: :unprocessable_entity, message: format(PRESET_INPUT_MESSAGE, source.display_name))
        rescue OauthPresetSetup::Conflict
          render_error(status: :conflict, message: PRESET_CONFLICT_MESSAGE)
        rescue Broker::ExchangeError, ActiveRecord::RecordInvalid
          render_error(status: :bad_gateway, message: PRESET_FAILED_MESSAGE)
        end

        private

        # client_secret_set reports only whether a client secret is stored.
        def record_payload(app, credentials_count: nil)
          super(app).merge(
            client_secret_set: app.client_secret.present?,
            redirect_uri: oauth_callback_redirect_uri(app.slug),
            start_url: oauth_start_url(app.slug),
            credentials_count: credentials_count || app.broker_credentials.count
          )
        end

        def minted_credential_payload(credential)
          {
            id: credential.oid,
            foreign_id: credential.foreign_id,
            name: credential.name,
            status: credential.status,
            provider_email: credential.provider_email,
            external_user_key: credential.external_user_key,
            created_at: credential.created_at
          }
        end
      end
    end
  end
end
