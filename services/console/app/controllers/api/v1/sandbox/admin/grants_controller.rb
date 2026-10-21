module Api
  module V1
    module Sandbox
      module Admin
        # Direct principal grants only. Role grants change every holder of the
        # role and stay with the operator API.
        class GrantsController < ::Api::V1::GrantsController
          include SandboxAdminAuthentication

          def create
            if data_params[:role_id].present?
              return render_error(status: :forbidden, message: "role grants are not available from a sandbox")
            end

            super
          end

          def destroy
            Grant.where.not(principal_id: nil).find_by_oid!(params[:id])
            super
          end
        end
      end
    end
  end
end
