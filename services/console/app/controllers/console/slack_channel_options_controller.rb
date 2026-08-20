module Console
  class SlackChannelOptionsController < ApplicationController
    MAX_RESULTS = 20

    before_action :require_admin

    def index
      response.headers["Cache-Control"] = "no-store"
      SlackChannelCatalogSync.enqueue_if_empty
      direct_message_option = scheduled_task_direct_message_option
      direct_message_options = direct_message_option ? [ direct_message_option ] : []
      channels = channel_scope.matching(params[:q]).ordered.limit(MAX_RESULTS - direct_message_options.length)

      render json: {
        options: direct_message_options + channels.map { |channel| channel_option(channel) },
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
      return "Connect your Slack account to choose a channel." if scheduled_task_delivery_policy.slack_user_id.blank?

      team_channels = SlackBotChannel.active.for_team(scheduled_task_delivery_policy.slack_team_id)
      if team_channels.where(membership_refreshed_at: nil).exists?
        "Slack channel memberships are loading."
      end
    end

    def find_owner
      case params[:owner_type]
      when "principal" then Principal.find_by_oid!(params[:id])
      when "role" then Role.find_by_oid!(params[:id])
      else raise ActiveRecord::RecordNotFound
      end
    end

    def scheduled_task_direct_message_option
      return unless params[:owner_type] == "scheduled_task"

      user_id = scheduled_task_delivery_policy.direct_message_user_id
      return if user_id.blank?

      label = direct_message_label
      searchable = [ "direct message", "dm", "me", label, user_id, scheduled_task_author.email ].join(" ").downcase
      return if params[:q].present? && !searchable.include?(params[:q].to_s.strip.downcase)

      {
        value: user_id,
        label: label,
        description: "#{user_id} · Slack DM"
      }
    end

    def direct_message_label
      return "Direct message to you" if scheduled_task_author == current_user

      "Direct message to #{scheduled_task_author.email}"
    end

    def scheduled_task_delivery_policy
      @scheduled_task_delivery_policy ||= SlackDeliveryPolicy.new(scheduled_task_author)
    end

    def scheduled_task_author
      @scheduled_task_author ||= if params[:task_id].present?
        ScheduledTask.find_by_oid!(params[:task_id]).author
      else
        current_user
      end
    end
  end
end
