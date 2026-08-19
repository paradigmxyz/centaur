require "digest"

class SlackChannelCatalogProvider
  FRESH_TTL = 5.minutes
  STALE_TTL = 24.hours
  ERROR_TTL = 30.seconds
  REFRESH_LOCK_TTL = 1.minute

  class << self
    def fetch
      config = configuration
      return unconfigured_result unless config

      key = cache_key(**config)
      payload = Rails.cache.read(key)
      enqueue_refresh(key) unless fresh?(payload)

      payload ? deserialize_result(payload) : loading_result
    end

    def search(query:, limit:, exclude_ids: [])
      result = fetch
      excluded = Array(exclude_ids).index_with(true)
      needle = query.to_s.strip.downcase
      channels = result.channels.reject { |channel| excluded.key?(channel.id) }
      if needle.present?
        channels = channels.select do |channel|
          channel.name.downcase.include?(needle) || channel.id.downcase.include?(needle)
        end
      end

      SlackChannelCatalog::Result.new(
        channels: channels.first(limit),
        error: result.error,
        configured: result.configured
      )
    end

    def refresh(cache_key:)
      config = configuration
      return unless config && cache_key == self.cache_key(**config)

      cached = Rails.cache.read(cache_key)
      result = SlackChannelCatalog.new(**config).fetch
      if result.ok?
        Rails.cache.write(cache_key, serialize_result(result), expires_in: STALE_TTL)
      elsif cached.nil?
        Rails.cache.write(cache_key, serialize_result(result), expires_in: ERROR_TTL)
      end
      result
    ensure
      Rails.cache.delete(refresh_lock_key(cache_key))
    end

    def cache_key(token:, api_url:)
      token_digest = Digest::SHA256.hexdigest(token)
      api_url_digest = Digest::SHA256.hexdigest(api_url)
      "slack_channel_catalog/v2/#{api_url_digest}/#{token_digest}"
    end

    private

    def configuration
      token = ENV["CENTAUR_CONSOLE_SLACK_BOT_TOKEN"].presence || ENV["SLACK_BOT_TOKEN"].presence
      return if token.blank?

      { token: token, api_url: ENV["SLACK_API_URL"].presence || SlackChannelCatalog::DEFAULT_API_URL }
    end

    def enqueue_refresh(cache_key)
      lock_key = refresh_lock_key(cache_key)
      return unless Rails.cache.write(lock_key, true, expires_in: REFRESH_LOCK_TTL, unless_exist: true)

      SlackChannelCatalogRefreshJob.perform_later(cache_key)
    rescue StandardError => e
      Rails.cache.delete(lock_key)
      Rails.logger.warn("Could not enqueue Slack channel catalog refresh: #{e.class}: #{e.message}")
    end

    def fresh?(payload)
      payload.is_a?(Hash) && payload["refreshed_at"].to_f > FRESH_TTL.ago.to_f
    end

    def refresh_lock_key(cache_key)
      "#{cache_key}/refreshing"
    end

    def serialize_result(result)
      {
        "channels" => result.channels.map do |channel|
          { "id" => channel.id, "name" => channel.name, "private" => channel.private }
        end,
        "error" => result.error,
        "configured" => result.configured,
        "refreshed_at" => Time.current.to_f
      }
    end

    def deserialize_result(payload)
      channels = Array(payload["channels"]).map do |channel|
        SlackChannelCatalog::Channel.new(
          id: channel.fetch("id"),
          name: channel.fetch("name"),
          private: channel.fetch("private")
        )
      end
      SlackChannelCatalog::Result.new(
        channels: channels,
        error: payload["error"],
        configured: payload["configured"]
      )
    end

    def loading_result
      SlackChannelCatalog::Result.new(
        channels: [],
        error: "Slack channel catalog is loading. Enter a channel ID or reload shortly.",
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
