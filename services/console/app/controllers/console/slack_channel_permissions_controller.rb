module Console
  class SlackChannelPermissionsController < ApplicationController
    layout "console"

    before_action :require_admin

    def destroy
      permission = SlackChannelPermission.find_by_oid!(params.require(:slack_channel_permission_id))
      redirect_path = owner_path(permission)

      permission.destroy!
      redirect_to redirect_path, notice: "Deleted Slack channel permission."
    end

    private

    def owner_path(permission)
      if permission.principal.present?
        console_principal_path(permission.principal.oid)
      else
        console_role_path(permission.role.oid)
      end
    end
  end
end
