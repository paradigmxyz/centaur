module Api
  module V1
    class RolesController < Api::BaseController
      credential_policy :bearer_or_session
      # Added for the console SPA; not part of the bearer API.
      credential_policy :session, only: %i[replace_slack_channel_permissions destroy_slack_channel_permission]

      include SlackChannelPermissionApi

      # Optional filters on top of labels and pagination: q (name or
      # foreign_id, case-insensitive) and exclude_principal (a principal oid;
      # omits the roles already assigned to it, for the assign-role picker).
      def index
        records, meta = paginated_label_search(filtered_roles(Role.includes(:slack_channel_permissions)))
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
      end

      def show
        role = Role.find_by_oid!(params[:id])
        render json: { data: record_payload(role) }
      end

      # GET /api/v1/roles/lookup/:foreign_id
      def lookup
        render json: { data: record_payload(find_by_foreign_id!(Role)) }
      end

      def create
        role = Role.new(foreign_id: data_params[:foreign_id], created_by: current_user)
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
      # is a foreign_id that is created when absent. foreign_id is immutable.
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

      private

      def filtered_roles(scope)
        if (oid = params[:exclude_principal].presence)
          principal = Principal.find_by_oid!(oid.to_s)
          scope = scope.where.not(id: principal.principal_roles.select(:role_id))
        end
        query = params[:q].to_s.strip
        if query.present?
          pattern = "%#{Role.sanitize_sql_like(query)}%"
          scope = scope.where("roles.name ILIKE :pattern OR roles.foreign_id ILIKE :pattern", pattern: pattern)
        end
        scope
      end

      def record_payload(role)
        {
          id: role.oid,
          foreign_id: role.foreign_id,
          name: role.name,
          labels: role.labels,
          slack_channel_permissions: role.slack_channel_permissions_payload,
          created_at: role.created_at,
          updated_at: role.updated_at
        }
      end

      def slack_channel_permission_owner
        Role.find_by_oid!(params[:id])
      end
    end
  end
end
