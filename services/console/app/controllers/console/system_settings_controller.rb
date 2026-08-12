module Console
  class SystemSettingsController < ApplicationController
    layout "console"

    before_action :require_admin
    before_action :set_system_setting

    def edit
    end

    def update
      @system_setting.assign_attributes(system_setting_params)
      return render :edit, status: :unprocessable_entity unless @system_setting.valid?

      ActiveRecord::Base.transaction do
        @system_setting.save!
        Role.replace_default_assignments!(selected_default_role_ids) if params[:system_setting]&.key?(:default_role_ids)
      end

      redirect_to edit_console_system_settings_path, notice: "System settings updated."
    end

    private

    def set_system_setting
      @system_setting = SystemSetting.current
      @roles = Role.order(:name, :foreign_id, :id)
    end

    def system_setting_params
      params.require(:system_setting).permit(
        :default_sandbox_repo_cache,
        :default_sandbox_observability_enabled,
        :default_sandbox_api_server_enabled
      )
    end

    def selected_default_role_ids
      @selected_default_role_ids ||= Array(params.dig(:system_setting, :default_role_ids)).filter_map do |value|
        Integer(value, exception: false)
      end.uniq
    end
  end
end
