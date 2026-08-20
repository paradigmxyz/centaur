module Console
  class SlackChannelOptionsController < ApplicationController
    MAX_RESULTS = 20

    before_action :require_admin

    def index
      response.headers["Cache-Control"] = "no-store"
      SlackChannelCatalogSync.enqueue_if_empty
      channels = channel_scope.matching(params[:q]).ordered.limit(MAX_RESULTS)

      render json: {
        options: channels.map { |channel| channel_option(channel) },
        error: catalog_error
      }
    end

    private

    def channel_scope
      return scheduled_task_delivery_policy.allowed_channels if params[:owner_type] == "scheduled_task"

      owner = find_owner
      SlackBotChannel.active
                     .excluding_channel_ids(owner.slack_channel_permissions.pluck(:channel_id))
    end

    def channel_option(channel)
      {
        value: channel.channel_id,
        label: "##{channel.name}",
        description: "#{channel.channel_id} · #{channel.private ? "Private" : "Public"}"
      }
    end

    def catalog_error
      return "SLACK_BOT_TOKEN is not configured." unless SlackChannelCatalogSync.configured?
      return "Slack channel catalog is loading. Enter a channel ID or reload shortly." if SlackBotChannel.none?
      return unless params[:owner_type] == "scheduled_task"
      return if scheduled_task_delivery_policy.slack_team_id.blank?

      team_channels = SlackBotChannel.active.for_team(scheduled_task_delivery_policy.slack_team_id).where(private: true)
      "Slack channel memberships are loading." if team_channels.where(membership_refreshed_at: nil).exists?
    end

    def find_owner
      case params[:owner_type]
      when "principal" then Principal.find_by_oid!(params[:id])
      when "role" then Role.find_by_oid!(params[:id])
      else raise ActiveRecord::RecordNotFound
      end
    end

    def scheduled_task_delivery_policy
      @scheduled_task_delivery_policy ||= SlackDeliveryPolicy.new(current_user)
    end
  end
end
