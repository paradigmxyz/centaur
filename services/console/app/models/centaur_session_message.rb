class CentaurSessionMessage < CentaurSessionRecord
  self.table_name = "session_messages"
  self.primary_key = "message_id"

  belongs_to :session,
             class_name: "CentaurSession",
             foreign_key: :thread_key,
             primary_key: :thread_key,
             inverse_of: :messages

  def readonly? = true

  def parts_array
    parts.is_a?(Array) ? parts : []
  end

  def metadata_hash
    metadata.is_a?(Hash) ? metadata : {}
  end
end
