class CentaurSession < CentaurSessionRecord
  self.table_name = "sessions"
  self.primary_key = "thread_key"

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

  def readonly? = true

  def metadata_hash
    metadata.is_a?(Hash) ? metadata : {}
  end
end
