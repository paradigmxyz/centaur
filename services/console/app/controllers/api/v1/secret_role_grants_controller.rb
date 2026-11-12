module Api
  module V1
    # The roles a secret is granted to, from the secret's side (the console
    # secret detail page). Grants are still Grant rows; these routes only scope
    # them to one secret so a revoke can't remove some other secret's grant.
    #
    #   GET    /api/v1/secrets/:kind/:secret_id/role_grants
    #   POST   /api/v1/secrets/:kind/:secret_id/role_grants      { data: { role_id } }
    #   DELETE /api/v1/secrets/:kind/:secret_id/role_grants/:id
    class SecretRoleGrantsController < Api::BaseController
      accepts_credentials :session

      before_action :set_secret

      # Role grants of the secret (by grant creation) plus the roles it is not
      # yet granted to, for the assign-role select.
      def index
        grants = role_grants.includes(:role).order(:id).to_a
        granted_ids = grants.map(&:role_id)
        assignable = Role.where.not(id: granted_ids).order(:id)
        render json: {
          data: grants.map { |grant| grant_payload(grant) },
          assignable_roles: assignable.map { |role| role_summary(role) }
        }
      end

      # Idempotent: an existing grant of this secret to the role returns 200.
      def create
        role = Role.find_by_oid!(data_params.require(:role_id))
        identity = { role: role, grantable_assoc => @secret }
        grant = Grant.create_with(created_by: current_user).find_or_create_by!(identity)
        render status: (grant.previously_new_record? ? :created : :ok), json: { data: grant_payload(grant) }
      rescue ActiveRecord::RecordNotUnique
        # A concurrent request already created the grant; report the end state.
        render status: :ok, json: { data: grant_payload(Grant.find_by!(identity)) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      def destroy
        role_grants.find_by_oid!(params[:id]).destroy!
        head :no_content
      end

      private

      def set_secret
        @secret = SecretKinds.find_secret!(params[:kind], params[:secret_id])
      end

      def grantable_assoc
        GrantableSummary.kind_for(@secret).then { |kind| :"#{kind}_secret" }
      end

      def role_grants
        Grant.where(grantable_assoc => @secret).where.not(role_id: nil)
      end

      def grant_payload(grant)
        {
          id: grant.oid,
          role_id: grant.role.oid,
          role: role_summary(grant.role),
          grantable: GrantableSummary.call(@secret),
          priority: grant.priority,
          created_at: grant.created_at,
          updated_at: grant.updated_at
        }
      end

      def role_summary(role)
        { id: role.oid, foreign_id: role.foreign_id, name: role.name }
      end
    end
  end
end
