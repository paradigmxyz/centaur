# Lets a sandbox use selected admin API endpoints on behalf of a Console admin,
# but only from that admin's Slack DM with the bot. The principal must be a
# user principal in the bot's own workspace (per the Slack channel catalog)
# linked to an active admin, and the proxy must be bound to that user's own
# Slack DM thread: the same principal also runs their MCP sessions and
# scheduled tasks, where no admin is present. Channel and Slack Connect
# principals are refused. Include into a subclass of an Api::V1 admin
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
    unless home_slack_team?(principal.slack_team_id) && serves_slack_dm?(principal) &&
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

  # api-rs sets the DM label from the session's thread key alone, never from
  # caller metadata, so API clients confined to their own namespace cannot
  # claim it.
  def serves_slack_dm?(principal)
    labels = current_proxy.labels.to_h
    principal.kind == Principal::USER_KIND && labels["centaur.slack_dm_channel_id"].present? &&
      principal.slack_user_id.present? && labels["centaur.slack_user_id"] == principal.slack_user_id
  end

  def home_slack_team?(team_id)
    team_id.present? && SlackBotChannel.exists?(team_id: team_id)
  end
end
