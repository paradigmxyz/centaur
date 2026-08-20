require "digest"

class SlackChannelCatalogProvider
  FRESH_TTL = 1.hour
  MEMBERSHIP_TTL = 24.hours
  MEMBERSHIP_RETRY_TTL = 1.hour
  MEMBERSHIP_BATCH_SIZE = 25
  REFRESH_LOCK_TTL = 1.minute

  class << self
    def fetch
      config = configuration
      return unconfigured_result unless config

      relation = active_scope
      state = state_for(config)
      enqueue_refresh(config) unless fresh?(relation, state)
      relation_result(relation.ordered, error: cold_error(relation, state))
    end

    def search(query:, limit:, exclude_ids: [], team_id: nil, member_user_ids: [])
      config = configuration
      return unconfigured_result unless config

      relation = active_scope
      state = state_for(config)
      enqueue_refresh(config) unless fresh?(relation, state)
      relation = relation.where(team_id: team_id) if team_id.present?
      relation = relation.where.not(channel_id: Array(exclude_ids)) if Array(exclude_ids).any?

      needle = query.to_s.strip
      if needle.present?
        escaped = ActiveRecord::Base.sanitize_sql_like(needle)
        relation = relation.where("name ILIKE :query OR channel_id ILIKE :query", query: "%#{escaped}%")
      end

      requested_members = Array(member_user_ids).map { |id| id.to_s.strip }.reject(&:blank?).uniq
      if requested_members.any?
        bot_user_id = state["bot_user_id"].presence || relation.limit(1).pick(:bot_user_id)
        required_members = ([ bot_user_id ] + requested_members).compact_blank.uniq
        relation = relation.where("member_user_ids @> ARRAY[?]::text[]", required_members)
      end

      relation_result(relation.ordered.limit(limit), error: cold_error(active_scope, state))
    end

    def names_for(channel_ids)
      config = configuration
      return {} unless config

      catalog_scope
        .where(channel_id: Array(channel_ids))
        .pluck(:channel_id, :name)
        .to_h
    end

    def refresh
      config = configuration
      return unconfigured_result unless config

      write_state(config, state_for(config).merge("last_attempted_at" => Time.current.to_f, "last_error" => nil))
      client = SlackChannelCatalog.new(token: config.fetch(:token), api_url: config.fetch(:api_url))
      identity = client.fetch_identity
      channels = client.fetch_channels
      persist_discovery!(config, identity, channels)
      fetch
    rescue SlackApi::RetryableError => e
      record_refresh_error(config, e.message)
      raise
    rescue StandardError => e
      record_refresh_error(config, e.message)
      SlackChannelCatalog::Result.new(channels: [], error: e.message, configured: true)
    ensure
      Rails.cache.delete(refresh_lock_key(config)) if config
    end

    def refresh_memberships(channel_id: nil)
      config = configuration
      return 0 unless config

      client = SlackChannelCatalog.new(token: config.fetch(:token), api_url: config.fetch(:api_url))
      identity = catalog_identity(config, client)
      channels = if channel_id.present?
        [ find_or_refresh_channel!(identity, client, channel_id) ].compact
      else
        stale_memberships(identity)
      end

      channels.each do |channel|
        refresh_channel_membership!(client, channel)
      rescue SlackApi::RetryableError
        channel.update!(membership_last_attempted_at: nil)
        raise
      rescue StandardError => e
        channel.update!(membership_last_attempted_at: Time.current, membership_error: e.message)
      end
      channels.length
    end

    def cache_key(token:, api_url:)
      token_digest = Digest::SHA256.hexdigest(token)
      api_url_digest = Digest::SHA256.hexdigest(api_url)
      "slack_channel_catalog/v3/#{api_url_digest}/#{token_digest}"
    end

    def configured?
      configuration.present?
    end

    private

    def configuration
      token = ENV["CENTAUR_CONSOLE_SLACK_BOT_TOKEN"].presence || ENV["SLACK_BOT_TOKEN"].presence
      return if token.blank?

      { token: token, api_url: ENV["SLACK_API_URL"].presence || SlackChannelCatalog::DEFAULT_API_URL }
    end

    def catalog_scope
      SlackBotChannel.all
    end

    def active_scope
      catalog_scope.active
    end

    def persist_discovery!(config, identity, channels)
      now = Time.current
      SlackBotChannel.transaction do
        catalog_scope.update_all(active: false, updated_at: now)
        channels.each do |remote|
          channel = SlackBotChannel.find_or_initialize_by(
            team_id: identity.team_id,
            channel_id: remote.id
          )
          channel.update!(
            bot_user_id: identity.bot_user_id,
            name: remote.name,
            private: remote.private,
            archived: remote.archived,
            active: !remote.archived,
            last_seen_at: now
          )
        end
      end
      write_state(
        config,
        {
          "team_id" => identity.team_id,
          "bot_user_id" => identity.bot_user_id,
          "last_attempted_at" => now.to_f,
          "last_succeeded_at" => now.to_f,
          "last_error" => nil
        }
      )
    end

    def catalog_identity(config, client)
      state = state_for(config)
      team_id = state["team_id"].presence
      bot_user_id = state["bot_user_id"].presence
      if team_id.blank? || bot_user_id.blank?
        team_id, bot_user_id = catalog_scope.limit(1).pick(:team_id, :bot_user_id)
      end
      return SlackChannelCatalog::Identity.new(team_id: team_id, bot_user_id: bot_user_id) if team_id && bot_user_id

      identity = client.fetch_identity
      write_state(config, state.merge("team_id" => identity.team_id, "bot_user_id" => identity.bot_user_id))
      identity
    end

    def find_or_refresh_channel!(identity, client, channel_id)
      remote = client.fetch_channel(channel_id)
      channel = SlackBotChannel.find_or_initialize_by(
        team_id: identity.team_id,
        channel_id: remote.id
      )
      channel.update!(
        bot_user_id: identity.bot_user_id,
        name: remote.name,
        private: remote.private,
        archived: remote.archived,
        active: !remote.archived,
        last_seen_at: Time.current
      )
      channel
    rescue SlackChannelCatalog::ChannelNotJoinedError
      catalog_scope.where(team_id: identity.team_id, channel_id: channel_id)
                   .update_all(active: false, updated_at: Time.current)
      nil
    end

    def stale_memberships(identity)
      catalog_scope.active
                   .where(team_id: identity.team_id, bot_user_id: identity.bot_user_id)
                   .where(
                     "membership_refreshed_at IS NULL OR membership_refreshed_at < ?",
                     MEMBERSHIP_TTL.ago
                   )
                   .where(
                     "membership_last_attempted_at IS NULL OR membership_last_attempted_at < ?",
                     MEMBERSHIP_RETRY_TTL.ago
                   )
                   .order(Arel.sql("membership_refreshed_at ASC NULLS FIRST"), :id)
                   .limit(MEMBERSHIP_BATCH_SIZE)
                   .to_a
    end

    def refresh_channel_membership!(client, channel)
      channel.update!(membership_last_attempted_at: Time.current, membership_error: nil)
      member_ids = client.fetch_member_user_ids(channel.channel_id)
      unless member_ids.include?(channel.bot_user_id)
        raise SlackApi::Error, "Slack membership for #{channel.channel_id} omitted the bot user."
      end

      channel.update!(
        member_user_ids: member_ids,
        membership_refreshed_at: Time.current,
        membership_error: nil
      )
    end

    def fresh?(relation, state)
      succeeded_at = state["last_succeeded_at"].to_f
      succeeded_at = relation.maximum(:last_seen_at)&.to_f if succeeded_at.zero?
      succeeded_at.present? && succeeded_at > FRESH_TTL.ago.to_f
    end

    def cold_error(relation, state)
      return if relation.exists? || state["last_succeeded_at"].to_f.positive?

      state["last_error"].presence || "Slack channel catalog is loading. Enter a channel ID or reload shortly."
    end

    def record_refresh_error(config, message)
      return unless config

      state = state_for(config)
      write_state(
        config,
        state.merge("last_attempted_at" => Time.current.to_f, "last_error" => message)
      )
    rescue StandardError => e
      Rails.logger.warn("Could not cache Slack channel catalog error: #{e.class}: #{e.message}")
    end

    def state_for(config)
      Rails.cache.read(state_cache_key(config)).to_h
    end

    def write_state(config, state)
      Rails.cache.write(state_cache_key(config), state)
    end

    def state_cache_key(config)
      "#{cache_key(**config)}/state"
    end

    def enqueue_refresh(config)
      lock_key = refresh_lock_key(config)
      return unless Rails.cache.write(lock_key, true, expires_in: REFRESH_LOCK_TTL, unless_exist: true)

      SlackChannelCatalogRefreshJob.perform_later
    rescue StandardError => e
      Rails.cache.delete(lock_key)
      Rails.logger.warn("Could not enqueue Slack channel catalog refresh: #{e.class}: #{e.message}")
    end

    def refresh_lock_key(config)
      "#{cache_key(**config)}/refreshing"
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
