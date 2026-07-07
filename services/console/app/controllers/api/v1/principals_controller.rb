module Api
  module V1
    class PrincipalsController < Api::BaseController
      def index
        records, meta = paginated_label_search(Principal.all)
        render json: { data: records.map { |p| record_payload(p) }, meta: meta }
      end

      # GET /api/v1/principals/:id
      #
      # :id is an opaque oid. To read by foreign_id, use the namespaced lookup
      # route (GET /api/v1/principals/lookup/:namespace/:foreign_id), which
      # requires the namespace explicitly rather than defaulting it.
      def show
        principal = Principal.find_by_oid!(params[:id])
        render json: { data: record_payload(principal) }
      end

      # GET /api/v1/principals/lookup/:namespace/:foreign_id
      def lookup
        render json: { data: record_payload(find_by_foreign_id!(Principal)) }
      end

      def create
        principal = Principal.new(namespace: upsert_namespace, foreign_id: data_params[:foreign_id],
                                  created_by: current_user)
        principal.assign_attributes(principal_params)
        principal.save!
        render status: :created, json: { data: record_payload(principal) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # PUT/PATCH upserts: an opaque id updates that record, any other identifier
      # is a foreign_id that is created when absent. namespace and foreign_id are
      # immutable, so they only take effect when the record is created.
      def update
        principal = resolve_for_upsert(Principal)
        was_new = principal.new_record?
        principal.assign_attributes(principal_params)
        principal.save!
        render status: (was_new ? :created : :ok), json: { data: record_payload(principal) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # GET /api/v1/principals/:id/effective_config
      # GET /api/v1/principals/lookup/:namespace/:foreign_id/effective_config
      #
      # Addressable by opaque oid (member route) or by an explicit namespace +
      # foreign_id (namespaced lookup route).
      #
      # The config this principal resolves to, in the same shape iron-proxy
      # receives on /sync, for operator inspection. Unlike /sync it never reveals
      # live secrets (inline control_plane values are redacted) and does no
      # config-hash negotiation. We send a content-derived ETag for change
      # detection but mark the response no-store, since it reflects mutable
      # grants and must never be served from a cache.
      def effective_config
        principal = params[:foreign_id].present? ? find_by_foreign_id!(Principal) : Principal.find_by_oid!(params[:id])
        body = { data: { id: principal.oid }.merge(principal.effective_config) }.to_json

        response.headers["ETag"] = %("#{Digest::SHA256.hexdigest(body)}")
        response.headers["Cache-Control"] = "no-store"
        render json: body
      end

      private

      def record_payload(principal)
        {
          id: principal.oid,
          namespace: principal.namespace,
          foreign_id: principal.foreign_id,
          name: principal.name,
          labels: principal.labels,
          sandbox_repo_cache: principal.sandbox_repo_cache,
          sandbox_observability_enabled: principal.sandbox_observability_enabled,
          sandbox_api_server_enabled: principal.sandbox_api_server_enabled,
          created_at: principal.created_at,
          updated_at: principal.updated_at
        }
      end

      def principal_params
        data_params.permit(
          :name,
          :sandbox_repo_cache,
          :sandbox_observability_enabled,
          :sandbox_api_server_enabled,
          labels: {}
        )
      end
    end
  end
end
