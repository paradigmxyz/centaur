module Api
  module V1
    module Console
      # GET /api/v1/console/slack_channels
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
      class SlackChannelsController < BaseController
        MAX_RESULTS = 20

        def index
          response.headers["Cache-Control"] = "no-store"
          SlackChannelCatalogSync.enqueue_if_empty
          render json: { data: channels.map { |channel| slack_channel_option(channel) }, meta: { catalog_error: catalog_error } }
        end

        private

        def channels
          ids = Array(params[:ids]).map { |id| id.to_s.strip.upcase }.compact_blank
          return SlackBotChannel.where(channel_id: ids.first(200)).ordered if ids.any?

          scope = SlackBotChannel.active
          if (owner = principal_or_role_param!(:exclude_owner))
            scope = scope.excluding_channel_ids(owner.slack_channel_permissions.pluck(:channel_id))
          end
          scope.matching(params[:q]).ordered.limit(MAX_RESULTS)
        end

        def catalog_error
          return "SLACK_BOT_TOKEN is not configured." unless SlackChannelCatalogSync.configured?

          "Slack channel catalog is loading. Enter a channel ID or reload shortly." if SlackBotChannel.none?
        end
      end
    end
  end
end
