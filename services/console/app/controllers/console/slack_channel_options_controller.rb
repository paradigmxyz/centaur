module Console
  class SlackChannelOptionsController < ApplicationController
    MAX_RESULTS = 20

    before_action :require_admin

    def index
      response.headers["Cache-Control"] = "no-store"
      owner = find_owner
      result = SlackChannelCatalogProvider.search(
        query: params[:q],
        limit: MAX_RESULTS,
        exclude_ids: owner.slack_channel_permissions.pluck(:channel_id)
      )

      render json: {
        options: result.channels.map do |channel|
          {
            value: channel.id,
            label: "##{channel.name}",
            description: "#{channel.id} · #{channel.private ? "Private" : "Public"}"
          }
        end,
        error: result.error
      }
    end

    private

    def find_owner
      case params[:owner_type]
      when "principal" then Principal.find_by_oid!(params[:id])
      when "role" then Role.find_by_oid!(params[:id])
      else raise ActiveRecord::RecordNotFound
      end
    end
  end
end
