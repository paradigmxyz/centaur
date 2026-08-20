class SlackChannelCatalogProvider
  REFRESH_LOCK_KEY = "slack_channel_catalog/refreshing".freeze
  REFRESH_LOCK_TTL = 1.minute

  class << self
    def fetch
      return unconfigured_result unless configured?

      relation = active_scope
      enqueue_sync if relation.none?
      relation_result(relation.ordered, error: loading_error(relation))
    end

    def search(query:, limit:, exclude_ids: [], team_id: nil, member_user_ids: [])
      return unconfigured_result unless configured?

      relation = active_scope
      enqueue_sync if relation.none?
      relation = relation.where(team_id: team_id) if team_id.present?
      relation = relation.where.not(channel_id: Array(exclude_ids)) if Array(exclude_ids).any?

      needle = query.to_s.strip
      if needle.present?
        escaped = ActiveRecord::Base.sanitize_sql_like(needle)
        relation = relation.where("name ILIKE :query OR channel_id ILIKE :query", query: "%#{escaped}%")
      end

      requested_members = Array(member_user_ids).map { |id| id.to_s.strip }.reject(&:blank?).uniq
      if requested_members.any?
        bot_user_id = relation.limit(1).pick(:bot_user_id)
        relation = if bot_user_id
          relation.where("member_user_ids @> ARRAY[?]::text[]", [ bot_user_id, *requested_members ].uniq)
        else
          relation.none
        end
      end

      relation_result(relation.ordered.limit(limit), error: loading_error(active_scope))
    end

    def names_for(channel_ids)
      return {} unless configured?

      SlackBotChannel.where(channel_id: Array(channel_ids)).pluck(:channel_id, :name).to_h
    end

    def configured?
      SlackChannelCatalogSync.configured?
    end

    private

    def active_scope
      SlackBotChannel.active
    end

    def enqueue_sync
      return unless Rails.cache.write(REFRESH_LOCK_KEY, true, expires_in: REFRESH_LOCK_TTL, unless_exist: true)

      SlackChannelCatalogRefreshJob.perform_later
    rescue StandardError => e
      Rails.cache.delete(REFRESH_LOCK_KEY)
      Rails.logger.warn("Could not enqueue Slack channel catalog sync: #{e.class}: #{e.message}")
    end

    def loading_error(relation)
      return if relation.exists?

      "Slack channel catalog is loading. Enter a channel ID or reload shortly."
    end

    def relation_result(relation, error: nil)
      SlackChannelCatalog::Result.new(
        channels: relation.map(&:catalog_channel),
        error: error,
        configured: true
      )
    end

    def unconfigured_result
      SlackChannelCatalog::Result.new(
        channels: [],
        error: "SLACK_BOT_TOKEN is not configured.",
        configured: false
      )
    end
  end
end
