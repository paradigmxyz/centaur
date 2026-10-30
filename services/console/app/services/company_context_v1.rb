# Switch for the Console's v1 company-context sync jobs: Slack DMs, Google Docs,
# and Granola. Disabling it stops those jobs, including already-queued ones,
# without deleting anything they synced.
module CompanyContextV1
  module_function

  def enabled?
    raw = ConsoleEnv["COMPANY_CONTEXT_V1_ENABLED"]
    raw.blank? || ActiveModel::Type::Boolean.new.cast(raw)
  end
end
