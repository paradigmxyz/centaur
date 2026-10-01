# api-rs owns these rows. A channel principal can serve several Crew apps, so
# self-management must also match the authenticated sandbox's current assignment.
class CrewSession < CentaurSessionRecord
  self.table_name = "sessions"
  self.primary_key = "thread_key"

  def readonly?
    true
  end

  def self.app_id_for(proxy)
    keys = where(sandbox_id: proxy.name, iron_control_principal: proxy.principal.oid)
      .limit(2).pluck(:thread_key)
    return unless keys.one?

    # Legacy Slack threads, web sessions and workflows carry no Crew identity.
    match = /\Aslack:T[A-Z0-9]+:(A[A-Z0-9]+):[CDG][A-Z0-9]+:\d+\.\d+\z/.match(keys.first)
    match && match[1]
  end
end
