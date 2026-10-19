module Api
  module V1
    module Sandbox
      module Admin
        class PrincipalsController < ::Api::V1::PrincipalsController
          include SandboxAdminAuthentication

          SANDBOX_ACCESS_FLAGS = %w[
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

          # GET /api/v1/sandbox/admin/principals/:id/effective_grants
          #
          # Secrets granted directly or through roles, as on the Console
          # principal page, with how each one is reached.
          def effective_grants
            principal = Principal.find_by_oid!(params[:id])
            limit = pagination_limit
            result = PrincipalEffectiveGrantsPage.new(
              principal: principal,
              relations: principal.granted_secrets_by_kind,
              page: pagination_page,
              per_page: limit
            ).call

            data = result.records_by_kind.flat_map do |kind, secrets|
              secrets.map do |secret|
                effective_grant_payload(kind, secret, result.sources_by_key[[ kind, secret.id ]])
              end
            end
            total_pages = result.total_count.zero? ? 0 : result.total_pages
            render json: { data: data, meta: { page: result.page, limit: limit,
                                               total: result.total_count, total_pages: total_pages } }
          end

          # PATCH /api/v1/sandbox/admin/principals/:id/sandbox_access
          #
          # Updates only the supplied sandbox capabilities, unlike the
          # document-replacing principal PUT.
          def update_sandbox_access
            principal = Principal.find_by_oid!(params[:id])
            principal.update!(sandbox_access_params)
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

          private

          def effective_grant_payload(kind, secret, sources)
            roles = sources.filter_map { |source| source[:role] }.uniq
            {
              id: secret.oid,
              grant_key: PrincipalEffectiveGrantsPage::GRANT_COLUMN_BY_KIND.fetch(kind),
              foreign_id: secret.foreign_id,
              name: secret.try(:name),
              direct: sources.any? { |source| source[:type] == :direct },
              roles: roles.map { |role| { id: role.oid, foreign_id: role.foreign_id, name: role.name } }
            }
          end

          # Only literal JSON booleans for flags and a string repo cache mode;
          # anything else is rejected rather than cast.
          def sandbox_access_params
            data = data_params
            raise ActionController::BadRequest, "data must be an object" unless data.is_a?(ActionController::Parameters)

            attributes = data.to_unsafe_h.slice("sandbox_repo_cache", *SANDBOX_ACCESS_FLAGS)
            raise ActionController::BadRequest, "provide at least one sandbox capability" if attributes.empty?

            invalid = attributes.reject do |key, value|
              key == "sandbox_repo_cache" ? value.is_a?(String) : [ true, false ].include?(value)
            end
            if invalid.any?
              raise ActionController::BadRequest,
                    "#{invalid.keys.join(', ')} must be true or false (sandbox_repo_cache: none, public, or all)"
            end

            attributes
          end
        end
      end
    end
  end
end
