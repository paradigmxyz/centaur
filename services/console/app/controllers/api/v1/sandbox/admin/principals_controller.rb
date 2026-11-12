module Api
  module V1
    module Sandbox
      module Admin
        class PrincipalsController < ::Api::V1::PrincipalsController
          include SandboxAdminAuthentication
          include PrincipalEffectiveGrants

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

          # POST /api/v1/sandbox/admin/principals
          #
          # Creates only Slack channel, Slack user, and workflow principals, with
          # the foreign ID api-rs derives at session start or for
          # WORKFLOW_PRINCIPAL = True, so the caller cannot choose a foreign ID,
          # kind, or Console-user link.
          def create
            principal = case data_params[:kind]
            when "slack_channel"
              id = data_params[:slack_channel_id].to_s.strip.upcase
              raise ActionController::BadRequest, "slack_channel_id must be a C… or G… channel ID" unless id.start_with?("C", "G")

              Principal.new(kind: "slack_channel", foreign_id: "slack-channel-#{id.downcase}",
                            name: "Slack Channel #{id}", slack_channel_id: id)
            when "slack_dm"
              id = data_params[:slack_user_id].to_s.strip.upcase
              team = current_proxy.principal.slack_team_id
              Principal.new(kind: "slack_dm", foreign_id: "slack-user-#{team.downcase}-#{id.downcase}",
                            name: "Slack User #{id} (team #{team})", slack_user_id: id, slack_team_id: team)
            when "workflow"
              name = data_params[:workflow_name]
              raise ActionController::BadRequest, "workflow_name must be a WORKFLOW_NAME" unless name.is_a?(String) && name.present?

              Principal.new(kind: "workflow", foreign_id: "workflow-#{workflow_slug(name)}", name: "Workflow #{name}",
                            labels: { "managed-by" => "centaur", "workflow_name" => name })
            else
              raise ActionController::BadRequest, "kind must be slack_channel, slack_dm, or workflow"
            end
            principal.created_by = current_user

            Principal.transaction do
              principal.apply_default_sandbox_capabilities!
              principal.save!
              # api-rs adds this only to principals it creates itself.
              if principal.slack_channel_id
                principal.slack_channel_permissions.create!(
                  channel_id: principal.slack_channel_id, **SlackChannelPermission::DEFAULT_ENABLED_ATTRIBUTES
                )
              end
            end
            render status: :created, json: { data: record_payload(principal) }
          rescue ActiveRecord::RecordInvalid => e
            render_validation_error(e.record)
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

          # Matches api-rs's slugify for workflow principal foreign IDs.
          def workflow_slug(name)
            name.downcase.gsub(/[^a-z0-9]+/, "-").delete_prefix("-").delete_suffix("-").presence || "x"
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
