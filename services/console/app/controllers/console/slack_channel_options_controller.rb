module Console
  class SlackChannelOptionsController < ApplicationController
    MAX_RESULTS = 20

    before_action :require_admin

    def index
      response.headers["Cache-Control"] = "no-store"
      owner = find_owner
      SlackChannelCatalogSync.enqueue_if_empty
      channels = SlackBotChannel.active
                                .excluding_channel_ids(owner.slack_channel_permissions.pluck(:channel_id))
                                .matching(params[:q])
                                .ordered
                                .limit(MAX_RESULTS)

      render json: {
        options: channels.map do |channel|
          {
            value: channel.channel_id,
            label: "##{channel.name}",
            description: "#{channel.channel_id} · #{channel.private ? "Private" : "Public"}"
          }
        end,
        error: catalog_error
      }
    end

    private

    def catalog_error
      return "SLACK_BOT_TOKEN is not configured." unless SlackChannelCatalogSync.configured?
      "Slack channel catalog is loading. Enter a channel ID or reload shortly." if SlackBotChannel.none?
    end

    def find_owner
      case params[:owner_type]
      when "principal" then Principal.find_by_oid!(params[:id])
      when "role" then Role.find_by_oid!(params[:id])
      else raise ActiveRecord::RecordNotFound
      end
    end
  end
end
