# Lets a sandbox use selected admin API endpoints on behalf of a Console admin,
# but only from that admin's Slack DM with the bot. The principal must be in
# the bot's own workspace (per the Slack channel catalog) and linked to an
# active admin. A Slack DM principal only ever serves DMs. A Console-user
# principal (which absorbs its user's Slack DM principal) also runs scheduled
# tasks and MCP sessions with no admin present, so it qualifies only on a
# proxy api-rs bound to that user's own Slack DM thread. Channel and Slack
# Connect principals are refused. Include into a subclass of an Api::V1 admin
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
    case principal.kind
    when "slack_dm" then true
    when "console_user"
      labels = current_proxy.labels.to_h
      labels["centaur.slack_dm_channel_id"].present? && principal.slack_user_id.present? &&
        labels["centaur.slack_user_id"] == principal.slack_user_id
    else false
    end
  end

  def home_slack_team?(team_id)
    team_id.present? && SlackBotChannel.exists?(team_id: team_id)
  end
end
