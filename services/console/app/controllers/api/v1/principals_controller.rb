module Api
  module V1
    class PrincipalsController < Api::BaseController
      InvalidSlackChannelPermissions = Class.new(StandardError)

      rescue_from InvalidSlackChannelPermissions, with: :render_slack_channel_permissions_error

      def index
        records, meta = paginated_label_search(Principal.includes(:slack_channel_permissions))
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
        ActiveRecord::Base.transaction do
          principal.assign_attributes(principal_params)
          principal.apply_default_sandbox_capabilities!(principal_params)
          principal.save!
          replace_slack_channel_permissions!(principal) if data_params.key?(:slack_channel_permissions)
        end
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
        ActiveRecord::Base.transaction do
          principal.assign_attributes(principal_params)
          principal.apply_default_sandbox_capabilities!(principal_params) if was_new
          principal.save!
          replace_slack_channel_permissions!(principal) if data_params.key?(:slack_channel_permissions)
        end
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

      # POST /api/v1/principals/:id/slack_channel_permissions
      #
      # Upserts one Slack channel permission row without replacing the rest of
      # the principal's operator-managed Slack permissions.
      def upsert_slack_channel_permission
        principal = Principal.find_by_oid!(params[:id])
        attrs = upsert_slack_channel_permission_params
        attrs[:channel_id] = attrs[:channel_id].to_s.strip.upcase
        permission, was_new = save_slack_channel_permission!(principal, attrs)

        render status: (was_new ? :created : :ok), json: { data: permission.as_permission_json }
      rescue ActiveRecord::RecordNotUnique
        permission = principal.slack_channel_permissions.find_by!(channel_id: attrs[:channel_id])
        permission.assign_attributes(attrs)
        permission.save!
        render status: :ok, json: { data: permission.as_permission_json }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      private

      def record_payload(principal)
        {
          id: principal.oid,
          namespace: principal.namespace,
          foreign_id: principal.foreign_id,
          name: principal.name,
          labels: principal.labels_with_sandbox_capabilities,
          slack_channel_permissions: principal.slack_channel_permissions_payload,
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

      def replace_slack_channel_permissions!(principal)
        SlackChannelPermission.replace_for_principal!(
          principal,
          slack_channel_permission_params
        )
      end

      def save_slack_channel_permission!(principal, attrs)
        permission = principal.slack_channel_permissions.find_or_initialize_by(
          channel_id: attrs[:channel_id]
        )
        was_new = permission.new_record?
        permission.assign_attributes(attrs)
        permission.save!
        [ permission, was_new ]
      end

      def slack_channel_permission_params
        raw = data_params[:slack_channel_permissions]
        unless raw.nil? || raw.is_a?(Array)
          raise InvalidSlackChannelPermissions, "slack_channel_permissions must be an array"
        end

        rows = data_params.permit(
          slack_channel_permissions: %i[
            channel_id
            channel_name
            upload_enabled
            download_enabled
            history_enabled
          ]
        ).fetch(:slack_channel_permissions, [])

        if raw.present? && rows.length != raw.length
          raise InvalidSlackChannelPermissions, "slack_channel_permissions rows must be objects"
        end

        rows
      end

      def upsert_slack_channel_permission_params
        @upsert_slack_channel_permission_params ||= data_params.permit(
          :channel_id,
          :channel_name,
          :upload_enabled,
          :download_enabled,
          :history_enabled
        ).tap do |attrs|
          attrs[:upload_enabled] = true unless attrs.key?(:upload_enabled)
          attrs[:download_enabled] = true unless attrs.key?(:download_enabled)
          attrs[:history_enabled] = true unless attrs.key?(:history_enabled)
        end
      end

      def render_slack_channel_permissions_error(error)
        render_error(status: :unprocessable_entity, message: error.message)
      end
    end
  end
end
