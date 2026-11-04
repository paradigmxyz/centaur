module Api
  module V1
    module Sandbox
      module Admin
        # The Slack Connect allowlist, one team id at a time. api-rs stores the
        # list and validates ids; the Console Settings page edits it whole.
        class SlackExternalTeamsController < ::Api::BaseController
          include SandboxAdminAuthentication

          class_attribute :api_client_factory, default: -> { CentaurApiClient.new(timeout: 5) }

          # Later handlers win, so bad input still renders 400.
          rescue_from StandardError, with: :render_api_error
          rescue_from ActionController::ParameterMissing, ActionController::BadRequest, with: :render_bad_request

          def index
            render json: { data: api_client.slack_external_team_ids }
          end

          def create
            data = data_params
            raise ActionController::BadRequest, "data must be an object" unless data.is_a?(ActionController::Parameters)

            team_id = normalized(data[:team_id])
            team_ids = api_client.slack_external_team_ids
            team_ids = api_client.replace_slack_external_team_ids(team_ids | [ team_id ]) unless team_ids.include?(team_id)
            render json: { data: team_ids }
          end

          def destroy
            team_ids = api_client.slack_external_team_ids
            team_id = normalized(params[:id])
            unless team_ids.include?(team_id)
              return render_error(status: :not_found, message: "#{team_id} is not an allowed Slack Connect workspace")
            end

            render json: { data: api_client.replace_slack_external_team_ids(team_ids - [ team_id ]) }
          end

          private

          def api_client
            @api_client ||= api_client_factory.call
          end

          def normalized(team_id)
            raise ActionController::BadRequest, "team_id must be a string" unless team_id.is_a?(String)

            team_id.strip.upcase
          end

          def render_api_error(error)
            return render_error(status: :unprocessable_entity, message: error.message) if error.try(:status) == 400

            Rails.logger.warn("sandbox_admin_slack_external_teams_failed error=#{error.class}: #{error.message}")
            render_error(status: :service_unavailable, message: "the Centaur API is unavailable")
          end
        end
      end
    end
  end
end
