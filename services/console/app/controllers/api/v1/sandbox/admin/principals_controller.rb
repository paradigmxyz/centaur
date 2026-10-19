module Api
  module V1
    module Sandbox
      module Admin
        class PrincipalsController < ::Api::V1::PrincipalsController
          include SandboxAdminAuthentication

          SANDBOX_ACCESS_ATTRIBUTES = %i[
            sandbox_repo_cache
            sandbox_observability_enabled
            sandbox_sessions_read_enabled
            sandbox_workflows_read_enabled
            sandbox_workflows_write_enabled
          ].freeze

          # Adds a slack_channel_id filter so an admin can find the principal
          # for the Slack channel they are talking about.
          def index
            channel_id = params[:slack_channel_id].to_s.strip.upcase
            return super if channel_id.blank?

            principals = Principal.where(slack_channel_id: channel_id).order(:id)
            render json: { data: principals.map { |principal| record_payload(principal) } }
          end

          # PATCH /api/v1/sandbox/admin/principals/:id/sandbox_access
          #
          # Updates only the supplied sandbox capabilities, unlike the
          # document-replacing principal PUT.
          def update_sandbox_access
            principal = Principal.find_by_oid!(params[:id])
            attributes = data_params.permit(*SANDBOX_ACCESS_ATTRIBUTES)
            raise ActionController::BadRequest, "provide at least one sandbox capability" if attributes.empty?

            principal.update!(attributes)
            render json: { data: record_payload(principal) }
          rescue ActiveRecord::RecordInvalid => e
            render_validation_error(e.record)
          end

          # DELETE /api/v1/sandbox/admin/principals/:id/slack_channel_permissions/:channel_id
          def destroy_slack_channel_permission
            principal = Principal.find_by_oid!(params[:id])
            principal.slack_channel_permissions.find_by!(channel_id: params[:channel_id].to_s.strip.upcase).destroy!
            head :no_content
          end
        end
      end
    end
  end
end
