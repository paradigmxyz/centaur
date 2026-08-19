class CentaurSession < CentaurSessionRecord
  self.table_name = "sessions"
  self.primary_key = "thread_key"

  SLACK_SOURCE_SQL = <<~SQL.squish.freeze
    thread_key LIKE 'slack:%'
    OR metadata ->> 'platform' = 'slack'
    OR metadata ->> 'source' = 'slackbotv2'
  SQL

  has_many :messages,
           class_name: "CentaurSessionMessage",
           foreign_key: :thread_key,
           primary_key: :thread_key,
           inverse_of: :session
  has_many :executions,
           class_name: "CentaurSessionExecution",
           foreign_key: :thread_key,
           primary_key: :thread_key,
           inverse_of: :session
  has_many :events,
           class_name: "CentaurSessionEvent",
           foreign_key: :thread_key,
           primary_key: :thread_key,
           inverse_of: :session

  scope :recent_first, -> { order(Arel.sql("coalesce(updated_at, created_at) desc"), :thread_key) }

  def self.public_slack_threads_enabled?
    ActiveModel::Type::Boolean.new.cast(ConsoleEnv.fetch("PUBLIC_SLACK_THREADS_ENABLED", false))
  end

  # Slack channel ID prefixes do not encode privacy: modern private channels
  # can also start with C. Treat the synchronized channel catalog as a positive
  # public allowlist, and fail closed until that catalog is available.
  def self.public_slack_channel_sql
    required_columns = %i[is_private is_syncable]
    return unless connection.data_source_exists?(:slack_sync_channels)
    return unless required_columns.all? { |column| connection.column_exists?(:slack_sync_channels, column) }

    <<~SQL.squish
      (#{SLACK_SOURCE_SQL})
      AND EXISTS (
        SELECT 1
        FROM slack_sync_channels
        WHERE slack_sync_channels.is_private = false
          AND slack_sync_channels.is_syncable = true
          AND slack_sync_channels.channel_id IN (
            split_part(sessions.thread_key, ':', 2),
            split_part(sessions.thread_key, ':', 3)
          )
      )
    SQL
  end

  def readonly? = true

  def metadata_hash
    metadata.is_a?(Hash) ? metadata : {}
  end
end
