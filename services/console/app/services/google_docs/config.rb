module GoogleDocs
  module Config
    module_function

    def sync_enabled?
      enabled?("GOOGLE_DOCS_SYNC_ENABLED")
    end

    def pdf_indexing_enabled?
      enabled?("GOOGLE_DRIVE_PDF_INDEXING_ENABLED")
    end

    def enabled?(name)
      raw = ConsoleEnv[name]
      raw.nil? ? false : ActiveModel::Type::Boolean.new.cast(raw)
    end
    private_class_method :enabled?
  end
end
