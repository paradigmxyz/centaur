module Console
  class SystemSettingsController < ApplicationController
    layout "console"

    class_attribute :api_client_factory, default: -> { CentaurApiClient.new(timeout: 5) }

    before_action :require_admin
    before_action :set_system_setting

    def edit
      load_slack_external_team_ids
    end

    def update
      @system_setting.assign_attributes(system_setting_params)
      unless @system_setting.valid?
        load_slack_external_team_ids
        return render :edit, status: :unprocessable_entity
      end

      ActiveRecord::Base.transaction do
        @system_setting.save!
        Role.replace_default_assignments!(selected_default_role_ids) if params[:system_setting]&.key?(:default_role_ids)
      end

      redirect_to edit_console_system_settings_path, notice: "System settings updated."
    end

    # The Slack Connect allowlist is stored by api-rs, which Slackbot reads.
    def slack_external_teams
      @slack_external_team_ids_text = params[:slack_external_team_ids].to_s
      api_client.replace_slack_external_team_ids(@slack_external_team_ids_text.split(/[\s,]+/).compact_blank)
      redirect_to edit_console_system_settings_path, notice: "Slack Connect workspaces updated."
    rescue StandardError => e
      @slack_external_teams_error = slack_external_teams_error(e)
      render :edit, status: (e.try(:status) == 400 ? :unprocessable_entity : :service_unavailable)
    end

    private

    def api_client
      @api_client ||= api_client_factory.call
    end

    def load_slack_external_team_ids
      @slack_external_team_ids_text = api_client.slack_external_team_ids.join("\n")
    rescue StandardError => e
      Rails.logger.warn("console_slack_external_team_ids_load_failed error=#{e.class}: #{e.message}")
      @slack_external_team_ids_text = nil
    end

    def slack_external_teams_error(error)
      return error.message if error.try(:status) == 400

      Rails.logger.warn("console_slack_external_team_ids_save_failed error=#{error.class}: #{error.message}")
      "Slack Connect workspaces could not be saved because the Centaur API is unavailable."
    end

    def set_system_setting
      @system_setting = SystemSetting.current
      @roles = Role.order(:name, :foreign_id, :id)
    end

    def system_setting_params
      params.require(:system_setting).permit(
        :default_sandbox_repo_cache,
        :default_sandbox_observability_enabled,
        :default_sandbox_sessions_read_enabled,
        :default_sandbox_workflows_read_enabled,
        :default_sandbox_workflows_write_enabled
      )
    end

    def selected_default_role_ids
      @selected_default_role_ids ||= Array(params.dig(:system_setting, :default_role_ids)).filter_map do |value|
        Integer(value, exception: false)
      end.uniq
    end
  end
end
