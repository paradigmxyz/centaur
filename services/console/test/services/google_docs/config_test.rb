require "test_helper"

module GoogleDocs
  class ConfigTest < ActiveSupport::TestCase
    test "sync ETL defaults to disabled and honors the feature switch" do
      env_key = "CENTAUR_CONSOLE_GOOGLE_DOCS_SYNC_ENABLED"
      legacy_env_key = "IRON_CONTROL_GOOGLE_DOCS_SYNC_ENABLED"
      previous = {
        env_key => ENV[env_key],
        legacy_env_key => ENV[legacy_env_key]
      }
      ENV.delete(env_key)
      ENV.delete(legacy_env_key)

      refute GoogleDocs::Config.sync_enabled?

      ENV[env_key] = "false"
      refute GoogleDocs::Config.sync_enabled?

      ENV[env_key] = "true"
      assert GoogleDocs::Config.sync_enabled?
    ensure
      previous.each do |key, value|
        if value.nil?
          ENV.delete(key)
        else
          ENV[key] = value
        end
      end
    end

    test "PDF indexing defaults to disabled and honors its feature switch" do
      env_key = "CENTAUR_CONSOLE_GOOGLE_DRIVE_PDF_INDEXING_ENABLED"
      previous = ENV[env_key]
      ENV.delete(env_key)

      refute GoogleDocs::Config.pdf_indexing_enabled?

      ENV[env_key] = "true"
      assert GoogleDocs::Config.pdf_indexing_enabled?
    ensure
      previous.nil? ? ENV.delete(env_key) : ENV[env_key] = previous
    end
  end
end
