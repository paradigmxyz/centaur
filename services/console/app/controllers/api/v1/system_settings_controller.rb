module Api
  module V1
    # Deployment-wide defaults for new control-plane records, and the Slack
    # Connect allowlist (stored by api-rs, read by Slackbot). Admin-only.
    #
    #   GET   /api/v1/system_settings                       defaults + every role's default flag
    #   PATCH /api/v1/system_settings                       update; default_role_ids replaces the set
    #   GET   /api/v1/system_settings/slack_external_teams  allowed external Slack team ids
    #   PUT   /api/v1/system_settings/slack_external_teams  replace the whole list
    class SystemSettingsController < Api::BaseController
      accepts_credentials :session

      class_attribute :api_client_factory, default: -> { CentaurApiClient.new(timeout: 5) }

      SETTING_FIELDS = %i[
        default_sandbox_repo_cache
        default_sandbox_observability_enabled
        default_sandbox_sessions_read_enabled
        default_sandbox_workflows_read_enabled
        default_sandbox_workflows_write_enabled
      ].freeze

      before_action :set_system_setting, only: %i[show update]

      def show
        render_settings
      end

      def update
        attrs = data_params
        @system_setting.assign_attributes(attrs.permit(*SETTING_FIELDS))
        role_ids = resolve_default_role_ids(attrs) if attrs.key?(:default_role_ids)
        return if performed?
        return render_validation_error(@system_setting) unless @system_setting.valid?

        ActiveRecord::Base.transaction do
          @system_setting.save!
          Role.replace_default_assignments!(role_ids) unless role_ids.nil?
        end
        render_settings
      end

      def slack_external_teams
        render json: { data: { slack_external_team_ids: api_client.slack_external_team_ids } }
      rescue StandardError => e
        Rails.logger.warn("console_slack_external_team_ids_load_failed error=#{e.class}: #{e.message}")
        render_error(status: :service_unavailable,
                     message: "The allowed external workspaces could not be loaded from the Centaur API.")
      end

      def replace_slack_external_teams
        ids = api_client.replace_slack_external_team_ids(requested_team_ids)
        render json: { data: { slack_external_team_ids: ids } }
      rescue StandardError => e
        raise if e.is_a?(ActionController::ParameterMissing)
        return render_error(status: :unprocessable_entity, message: e.message) if e.try(:status) == 400

        Rails.logger.warn("console_slack_external_team_ids_save_failed error=#{e.class}: #{e.message}")
        render_error(status: :service_unavailable,
                     message: "Slack Connect workspaces could not be saved because the Centaur API is unavailable.")
      end

      private

      def api_client
        @api_client ||= api_client_factory.call
      end

      def set_system_setting
        @system_setting = SystemSetting.current
      end

      # Role oids to assign by default; [] clears them. Unknown oids are a 422
      # so a stale form cannot silently drop a selection.
      def resolve_default_role_ids(attrs)
        oids = Array(attrs[:default_role_ids]).map(&:to_s).compact_blank.uniq
        ids = oids.index_with { |oid| Role.decode_oid(oid) }
        found = Role.where(id: ids.values.compact).pluck(:id)
        missing = oids.reject { |oid| found.include?(ids[oid]) }
        if missing.any?
          render_error(status: :unprocessable_entity, message: "validation failed",
                       details: { default_role_ids: [ "contains unknown roles: #{missing.join(', ')}" ] })
          return
        end
        found
      end

      # A list, or one string split on whitespace and commas (the textarea form).
      def requested_team_ids
        raw = data_params[:slack_external_team_ids]
        list = raw.is_a?(Array) ? raw.map(&:to_s) : raw.to_s.split(/[\s,]+/)
        list.map(&:strip).compact_blank
      end

      def render_settings
        roles = Role.order(:name, :foreign_id, :id)
        render json: {
          data: SETTING_FIELDS.index_with { |field| @system_setting.public_send(field) }.merge(
            default_role_ids: roles.select(&:assign_by_default?).map(&:oid),
            roles: roles.map do |role|
              { id: role.oid, foreign_id: role.foreign_id, name: role.name, assign_by_default: role.assign_by_default? }
            end,
            updated_at: @system_setting.updated_at
          )
        }
      end
    end
  end
end
