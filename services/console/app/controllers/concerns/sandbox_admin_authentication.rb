# Lets a sandbox use selected admin API endpoints on behalf of a Console admin,
# but only from that admin's Slack DM with the bot. The principal must be a
# Slack DM principal in the bot's own workspace (per the Slack channel catalog)
# linked to an active admin. Channel, Slack Connect, and Console-user principals
# are refused; Console-user principals also run scheduled tasks and MCP
# sessions with no admin present. Include into a subclass of an Api::V1 admin
# controller.
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

    principal = current_proxy.principal
    user = principal.console_user
    unless principal.kind == "slack_dm" && home_slack_team?(principal.slack_team_id) &&
        user&.active? && user.admin?
      return render_error(
        status: :forbidden,
        message: "admin changes require a Slack DM from an active Console admin in the bot's workspace"
      )
    end

    @sandbox_admin = user
    Rails.logger.info(
      "sandbox_admin_request admin=#{user.oid} principal=#{principal.oid} " \
      "proxy=#{current_proxy.oid} method=#{request.method} path=#{request.path}"
    )
  end

  def home_slack_team?(team_id)
    team_id.present? && SlackBotChannel.exists?(team_id: team_id)
  end
end
