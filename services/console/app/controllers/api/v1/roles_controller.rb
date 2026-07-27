module Api
  module V1
    class RolesController < Api::BaseController
      InvalidSlackChannelPermissions = Class.new(StandardError)

      rescue_from InvalidSlackChannelPermissions, with: :render_slack_channel_permissions_error

      def index
        records, meta = paginated_label_search(Role.includes(:slack_channel_permissions))
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
      end

      def show
        role = Role.find_by_oid!(params[:id])
        render json: { data: record_payload(role) }
      end

      # GET /api/v1/roles/lookup/:namespace/:foreign_id
      def lookup
        render json: { data: record_payload(find_by_foreign_id!(Role)) }
      end

      def create
        role = Role.new(namespace: upsert_namespace, foreign_id: data_params[:foreign_id],
                        created_by: current_user)
        ActiveRecord::Base.transaction do
          role.assign_attributes(data_params.permit(:name, labels: {}))
          role.save!
          replace_slack_channel_permissions!(role) if data_params.key?(:slack_channel_permissions)
        end
        render status: :created, json: { data: record_payload(role) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # PUT/PATCH upserts: an opaque id updates that record, any other identifier
      # is a foreign_id that is created when absent. namespace and foreign_id are
      # immutable, so they only take effect when the record is created.
      def update
        role = resolve_for_upsert(Role)
        was_new = role.new_record?
        ActiveRecord::Base.transaction do
          role.assign_attributes(data_params.permit(:name, labels: {}))
          role.save!
          replace_slack_channel_permissions!(role) if data_params.key?(:slack_channel_permissions)
        end
        render status: (was_new ? :created : :ok), json: { data: record_payload(role) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      def destroy
        role = Role.find_by_oid!(params[:id])
        role.destroy!
        head :no_content
      end

      # POST /api/v1/roles/:id/slack_channel_permissions
      def upsert_slack_channel_permission
        role = Role.find_by_oid!(params[:id])
        attrs = upsert_slack_channel_permission_params
        attrs[:channel_id] = attrs[:channel_id].to_s.strip.upcase
        permission, was_new = save_slack_channel_permission!(role, attrs)

        render status: (was_new ? :created : :ok), json: { data: permission.as_permission_json }
      rescue ActiveRecord::RecordNotUnique
        permission = role.slack_channel_permissions.find_by!(channel_id: attrs[:channel_id])
        permission.assign_attributes(attrs)
        permission.save!
        render status: :ok, json: { data: permission.as_permission_json }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      private

      def record_payload(role)
        {
          id: role.oid,
          namespace: role.namespace,
          foreign_id: role.foreign_id,
          name: role.name,
          labels: role.labels,
          slack_channel_permissions: role.slack_channel_permissions_payload,
          created_at: role.created_at,
          updated_at: role.updated_at
        }
      end

      def replace_slack_channel_permissions!(role)
        SlackChannelPermission.replace_for_role!(role, slack_channel_permission_params)
      end

      def save_slack_channel_permission!(role, attrs)
        permission = role.slack_channel_permissions.find_or_initialize_by(
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
