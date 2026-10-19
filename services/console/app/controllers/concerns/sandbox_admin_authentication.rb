# Lets a sandbox use selected admin API endpoints on behalf of the Console admin
# its principal is linked to: a Slack DM with that admin, or that admin's
# Console session. Channel and other unlinked principals are refused, so
# permission changes can only be requested in a direct conversation with an
# active admin. Include into a subclass of an Api::V1 admin controller.
module SandboxAdminAuthentication
  extend ActiveSupport::Concern
  include SandboxTokenAuthentication

  included do
    skip_before_action :authenticate_api_key!
    before_action :authenticate_sandbox_admin!
  end

  def current_user
    @sandbox_admin
  end

  private

  def authenticate_sandbox_admin!
    authenticate_sandbox_token!
    return if performed?

    user = current_proxy.principal.console_user
    unless user&.active? && user.admin?
      return render_error(status: :forbidden, message: "sandbox principal is not linked to an active Console admin")
    end

    @sandbox_admin = user
    Rails.logger.info(
      "sandbox_admin_request admin=#{user.oid} principal=#{current_proxy.principal.oid} " \
      "proxy=#{current_proxy.oid} method=#{request.method} path=#{request.path}"
    )
  end
end
