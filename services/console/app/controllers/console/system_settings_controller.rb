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
        @system_setting.publish_organization_instructions!(published_by: current_user) if publish_organization_instructions?
      end

      redirect_to edit_console_system_settings_path, notice: update_notice
    end

    def restore_organization_instructions
      version = OrganizationInstructionVersion.find_by_oid!(params[:id])
      @system_setting.restore_organization_instructions!(version: version, published_by: current_user)
      redirect_to edit_console_system_settings_path,
                  notice: "Organization instructions restored and published."
    end

    private

    def set_system_setting
      @system_setting = SystemSetting.current
      @roles = Role.order(:name, :foreign_id, :id)
      @organization_instruction_versions = OrganizationInstructionVersion.includes(:published_by).order(id: :desc).limit(10)
    end

    def system_setting_params
      params.require(:system_setting).permit(
        :default_sandbox_repo_cache,
        :default_sandbox_observability_enabled,
        :default_sandbox_sessions_read_enabled,
        :default_sandbox_workflows_read_enabled,
        :default_sandbox_workflows_write_enabled,
        :organization_instructions_draft
      )
    end

    def publish_organization_instructions?
      params[:publish_organization_instructions] == "1"
    end

    def update_notice
      return "Organization instructions published." if publish_organization_instructions?
      return "Draft saved." if params.dig(:system_setting, :organization_instructions_draft)

      "System settings updated."
    end

    def selected_default_role_ids
      @selected_default_role_ids ||= Array(params.dig(:system_setting, :default_role_ids)).filter_map do |value|
        Integer(value, exception: false)
      end.uniq
    end
  end
end
