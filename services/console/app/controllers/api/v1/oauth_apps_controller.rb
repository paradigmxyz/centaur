module Api
  module V1
    # Operator CRUD for OAuth apps. An app's whole identity is its globally-unique
    # `slug`, so it is addressed by oid or slug (no namespace/foreign_id), and
    # `PUT` upserts by slug. client_secret is required and write-only: it is
    # accepted on writes but NEVER serialized back.
    class OauthAppsController < Api::BaseController
      def index
        records, meta = paginated_apps
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
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

      # List all apps (no namespace scoping), with optional label filtering and
      # pagination. Reuses the base helpers but without the required namespace.
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
                            :credential_namespace, :enabled, labels: {}, allowed_scopes: [])
        # A PUT upsert by slug sets the slug before assignment; a blank body value
        # must not wipe it.
        base.delete(:slug) if base[:slug].blank? && app.slug.present?
        # client_secret is write-only: only assign when supplied, so a partial
        # update leaves the stored secret in place.
        base.delete(:client_secret) if base[:client_secret].blank?

        app.assign_attributes(base)
        app.save!
      end

      # Observability only. The client_secret is deliberately never included (it is
      # required, so its presence is implied).
      def record_payload(app)
        {
          id: app.oid,
          slug: app.slug,
          description: app.description,
          labels: app.labels,
          provider: app.provider,
          client_id: app.client_id,
          allowed_scopes: app.allowed_scopes,
          credential_namespace: app.credential_namespace,
          enabled: app.enabled,
          created_at: app.created_at,
          updated_at: app.updated_at
        }
      end
    end
  end
end
