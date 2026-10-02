# api-rs owns these rows. A channel principal can serve several Crew apps, so
# self-management must also match the authenticated sandbox's current assignment.
class CrewSession < CentaurSessionRecord
  self.table_name = "sessions"
  self.primary_key = "thread_key"

  def readonly?
    true
  end

  def self.identity_for(proxy)
    keys = where(sandbox_id: proxy.name, iron_control_principal: proxy.principal.oid)
      .limit(2).pluck(:thread_key)
    return unless keys.one?

    # Legacy Slack threads, web sessions and workflows carry no Crew identity.
    match = /\Aslack:T[A-Z0-9]+:(A[A-Z0-9]+):([CDG][A-Z0-9]+):\d+\.\d+\z/.match(keys.first)
    { app_id: match[1], conversation_id: match[2], thread_key: keys.first } if match
  end
end
