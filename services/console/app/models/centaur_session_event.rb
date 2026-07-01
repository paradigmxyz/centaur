class CentaurSessionEvent < CentaurSessionRecord
  self.table_name = "session_events"
  self.primary_key = "event_id"

  belongs_to :session,
             class_name: "CentaurSession",
             foreign_key: :thread_key,
             primary_key: :thread_key,
             inverse_of: :events

  def readonly? = true

  def payload_hash
    payload.is_a?(Hash) ? payload : {}
  end
end
