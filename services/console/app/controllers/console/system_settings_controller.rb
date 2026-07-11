module Console
  class SystemSettingsController < ApplicationController
    layout "console"

    before_action :require_admin
    before_action :set_system_setting

    def edit
    end

    def update
      if @system_setting.update(system_setting_params)
        redirect_to edit_console_system_settings_path, notice: "System settings updated."
      else
        render :edit, status: :unprocessable_entity
      end
    end

    private

    def set_system_setting
      @system_setting = SystemSetting.current
    end

    def system_setting_params
      params.require(:system_setting).permit(
        :default_sandbox_repo_cache,
        :default_sandbox_observability_enabled,
        :default_sandbox_api_server_enabled
      )
    end
  end
end
