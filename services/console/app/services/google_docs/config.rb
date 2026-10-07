module GoogleDocs
  module Config
    module_function

    def sync_enabled?
      return false unless CompanyContextV1.enabled?

      raw = ConsoleEnv["GOOGLE_DOCS_SYNC_ENABLED"]
      raw.nil? ? false : ActiveModel::Type::Boolean.new.cast(raw)
    end
  end
end
