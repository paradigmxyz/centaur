class CentaurSessionExecution < CentaurSessionRecord
  self.table_name = "session_executions"
  self.primary_key = "execution_id"

  belongs_to :session,
             class_name: "CentaurSession",
             foreign_key: :thread_key,
             primary_key: :thread_key,
             inverse_of: :executions

  def readonly? = true
end
