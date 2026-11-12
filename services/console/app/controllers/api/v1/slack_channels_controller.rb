module Api
  module V1
    # GET /api/v1/slack_channels
    #
    # Searches the Slack channel catalog for the console's channel pickers.
    # Admin-only. Returns at most 20 active channels.
    #
    # Query params:
    #   q              name or channel ID fragment
    #   exclude_owner  "principal:<oid>" or "role:<oid>"; omits channels that
    #                  owner already has a permission row for
    #   ids[]          resolve these channel IDs (names for existing rows);
    #                  ignores q/exclude_owner and includes archived channels
    #
    # meta.catalog_error explains an empty or loading catalog.
    class SlackChannelsController < Api::BaseController
      credential_policy :session

      MAX_RESULTS = 20

      def index
        response.headers["Cache-Control"] = "no-store"
        SlackChannelCatalogSync.enqueue_if_empty
        render json: { data: channels.map { |channel| channel_payload(channel) }, meta: { catalog_error: catalog_error } }
      end

      private

      def channels
        ids = Array(params[:ids]).map { |id| id.to_s.strip.upcase }.compact_blank
        return SlackBotChannel.where(channel_id: ids.first(200)).ordered if ids.any?

        scope = SlackBotChannel.active
        if (owner = exclude_owner)
          scope = scope.excluding_channel_ids(owner.slack_channel_permissions.pluck(:channel_id))
        end
        scope.matching(params[:q]).ordered.limit(MAX_RESULTS)
      end

      def exclude_owner
        raw = params[:exclude_owner].presence
        return unless raw

        type, oid = raw.to_s.split(":", 2)
        case type
        when "principal" then Principal.find_by_oid!(oid)
        when "role" then Role.find_by_oid!(oid)
        else raise ActionController::BadRequest, "exclude_owner must be principal:<oid> or role:<oid>"
        end
      end

      def channel_payload(channel)
        {
          channel_id: channel.channel_id,
          name: channel.name,
          private: channel.private,
          label: "##{channel.name}",
          description: "#{channel.channel_id} · #{channel.private ? "Private" : "Public"}"
        }
      end

      def catalog_error
        return "SLACK_BOT_TOKEN is not configured." unless SlackChannelCatalogSync.configured?

        "Slack channel catalog is loading. Enter a channel ID or reload shortly." if SlackBotChannel.none?
      end
    end
  end
end
