module Api
  module V1
    # Operator CRUD for OAuth apps. An app's whole identity is its globally-unique
    # `slug`, so it is addressed by oid or slug (no namespace/foreign_id), and
    # `PUT` upserts by slug. client_secret is required and write-only: it is
    # accepted on writes but NEVER serialized back.
    class OauthAppsController < Api::BaseController
      credential_policy :bearer_or_session
      # Added for the console SPA; not part of the bearer API.
      credential_policy :session, only: %i[preset credentials provider_details]

      include OauthAppUrls

      # Builds the RFC 7591 client used by #preset (tests swap it).
      class_attribute :registration_client_factory, default: -> { Oauth::DynamicRegistration.new }

      PRESET_CONFLICT_MESSAGE = "The preset conflicts with an existing OAuth app. Review its configuration.".freeze
      PRESET_FAILED_MESSAGE = "Could not set up the integration. Please try again or review the OAuth app configuration.".freeze

      def index
        records, meta = paginated_apps
        counts = BrokerCredential.where(oauth_app_id: records.map(&:id)).group(:oauth_app_id).count
        render json: { data: records.map { |r| record_payload(r, credentials_count: counts.fetch(r.id, 0)) }, meta: meta }
      end

      def show
        render json: { data: record_payload(OauthApp.find_by_oid!(params[:id])) }
      end

      # GET /api/v1/oauth_apps/lookup/:slug
      def lookup
        app = OauthApp.find_by!(slug: params.require(:slug))
        render json: { data: record_payload(app) }
      end

      def create
        app = OauthApp.new(created_by: current_user)
        assign_and_save!(app, data_params)
        render status: :created, json: { data: record_payload(app) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      def update
        app = resolve_app_for_upsert
        was_new = app.new_record?
        assign_and_save!(app, data_params)
        render status: (was_new ? :created : :ok), json: { data: record_payload(app) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      def destroy
        app = OauthApp.find_by_oid!(params[:id])
        app.destroy!
        head :no_content
      rescue ActiveRecord::RecordNotDestroyed
        render status: :conflict, json: { error: { message: app.errors.full_messages.to_sentence } }
      end

      # GET /api/v1/oauth_apps/:id/credentials
      #
      # The broker credentials this app minted, oldest first. A summary only:
      # the token blob is never serialized (see /api/v1/broker_credentials/:id).
      def credentials
        app = OauthApp.find_by_oid!(params[:id])
        records, meta = paginated_label_search(app.broker_credentials)
        render json: { data: records.map { |r| minted_credential_payload(r) }, meta: meta }
      end

      # GET /api/v1/oauth_apps/:id/provider_details
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

      # POST /api/v1/oauth_apps/presets/:provider
      #
      # Sets up a provider preset (registering an OAuth client with the provider
      # when the preset's app doesn't exist yet) and returns the consent start
      # URL to send the admin to.
      def preset
        provider = Oauth::Providers.fetch(params[:provider])
        return render_error(status: :not_found, message: "no preset for this provider") unless OauthPresetSetup.supported?(provider)

        app = OauthPresetSetup.new(
          provider: provider, user: current_user,
          redirect_uri_for: method(:oauth_callback_redirect_uri),
          registration_client: registration_client_factory.call
        ).call
        render json: { data: { id: app.oid, slug: app.slug, start_url: oauth_start_url(app.slug) } }
      rescue OauthPresetSetup::Conflict
        render_error(status: :conflict, message: PRESET_CONFLICT_MESSAGE)
      rescue Broker::ExchangeError, ActiveRecord::RecordInvalid
        render_error(status: :bad_gateway, message: PRESET_FAILED_MESSAGE)
      end

      private

      # Resolves a PUT/PATCH target. An opaque id must reference an existing app
      # (update only); any other value is treated as a slug and the app is
      # initialized when absent, so a PUT to a slug creates it. The slug column is
      # set from the URL here rather than mass assignment, and a slug can never
      # start with the opaque-id prefix (model validation), so the two identifier
      # forms stay unambiguous.
      def resolve_app_for_upsert
        identifier = params[:id].to_s
        if identifier.start_with?("#{OauthApp.oid_prefix}_")
          OauthApp.find_by_oid!(identifier)
        else
          app = OauthApp.find_or_initialize_by(slug: identifier)
          app.created_by = current_user if app.new_record?
          app
        end
      end

      # List all apps with optional label filtering and pagination.
      def paginated_apps
        scope = OauthApp.all
        labels = label_filter_params
        scope = scope.where("labels @> ?", labels.to_json) if labels.any?

        limit = pagination_limit
        page = pagination_page
        total = scope.count
        records = scope.order(created_at: :asc, id: :asc).limit(limit).offset((page - 1) * limit)

        total_pages = total.zero? ? 0 : ((total + limit - 1) / limit)
        [ records, { page: page, limit: limit, total: total, total_pages: total_pages } ]
      end

      def assign_and_save!(app, attrs)
        base = attrs.permit(:slug, :description, :provider, :client_id, :client_secret,
                            :enabled, :always_available, :shared, labels: {}, allowed_scopes: [],
                            provider_config: {})
        # A PUT upsert by slug sets the slug before assignment; a blank body value
        # must not wipe it.
        base.delete(:slug) if base[:slug].blank? && app.slug.present?
        # client_secret is write-only: only assign when supplied, so a partial
        # update leaves the stored secret in place.
        base.delete(:client_secret) if base[:client_secret].blank?

        app.assign_attributes(base)
        app.save!
      end

      # Observability only. The client_secret is deliberately never included;
      # client_secret_set reports only whether one is stored.
      def record_payload(app, credentials_count: nil)
        {
          id: app.oid,
          slug: app.slug,
          description: app.description,
          labels: app.labels,
          provider: app.provider,
          client_id: app.client_id,
          allowed_scopes: app.allowed_scopes,
          provider_config: app.provider_config,
          enabled: app.enabled,
          always_available: app.always_available,
          shared: app.shared,
          client_secret_set: app.client_secret.present?,
          redirect_uri: oauth_callback_redirect_uri(app.slug),
          start_url: oauth_start_url(app.slug),
          credentials_count: credentials_count || app.broker_credentials.count,
          created_at: app.created_at,
          updated_at: app.updated_at
        }
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
