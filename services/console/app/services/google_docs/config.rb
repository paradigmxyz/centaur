module GoogleDocs
  module Config
    module_function

    def sync_enabled?
      raw = ConsoleEnv["GOOGLE_DOCS_SYNC_ENABLED"]
      raw.nil? ? false : ActiveModel::Type::Boolean.new.cast(raw)
    end
  end
end
