module Api
  module V1
    # Manages role assignments for a principal:
    #   GET    /api/v1/principals/:principal_id/roles
    #   POST   /api/v1/principals/:principal_id/roles      (body: data: { role_id })
    #   DELETE /api/v1/principals/:principal_id/roles/:id  (:id is the role oid)
    class PrincipalRolesController < Api::BaseController
      credential_policy :bearer_or_session

      def index
        principal = Principal.find_by_oid!(params[:principal_id])
        roles = principal.roles.includes(:slack_channel_permissions).order(:id)
        render json: { data: roles.map { |r| role_payload(r) } }
      end

      # Idempotent: re-assigning a role the principal already holds returns the
      # existing assignment with 200 rather than a uniqueness 422. Concurrent
      # assignments of the same role are race-safe: whichever request loses the
      # unique-index race (or the uniqueness validation) reloads the winning row
      # and reports it with 200 instead of a 500 or 422.
      def create
        principal = Principal.find_by_oid!(params[:principal_id])
        role = Role.find_by_oid!(data_params.require(:role_id))
        assignment = principal.principal_roles.find_or_create_by!(role: role)
        render status: assignment.previously_new_record? ? :created : :ok, json: { data: role_payload(role) }
      rescue ActiveRecord::RecordNotUnique
        render_existing_assignment(principal, role)
      rescue ActiveRecord::RecordInvalid => e
        if principal.principal_roles.exists?(role: role)
          render_existing_assignment(principal, role)
        else
          render_validation_error(e.record)
        end
      end

      def destroy
        principal = Principal.find_by_oid!(params[:principal_id])
        role = Role.find_by_oid!(params[:id])
        assignment = principal.principal_roles.find_by!(role: role)
        assignment.destroy!
        head :no_content
      end

      private

      def render_existing_assignment(principal, role)
        principal.principal_roles.find_by!(role: role)
        render status: :ok, json: { data: role_payload(role.reload) }
      end

      def role_payload(role)
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
    end
  end
end
