require "test_helper"

# Fix 7: when CENTAUR_CONSOLE_CENTAUR_DATABASE_URL is unset, the session DB
# config falls back to the primary config with the database name overridden to
# ai_v2. The primary config commonly carries a :url (single-URL dev setup); a
# database path inside that url used to override the ai_v2 name because Rails'
# UrlConfig merges url-derived keys over sibling hash keys. The fallback must
# resolve the url into discrete params, drop :url, and force the ai_v2 name.
class CentaurSessionRecordTest < ActiveSupport::TestCase
  def build_config
    CentaurSessionRecord.send(:session_database_configuration)
  end

  def with_env(overrides)
    originals = {}
    overrides.each do |key, value|
      originals[key] = ENV[key]
      if value.nil?
        ENV.delete(key)
      else
        ENV[key] = value
      end
    end
    yield
  ensure
    originals.each { |key, value| value.nil? ? ENV.delete(key) : ENV[key] = value }
  end

  def stub_primary(config)
    CentaurSessionRecord.stub(:primary_database_configuration, config) { yield }
  end

  test "fallback targets ai_v2 even when the primary url has another database path" do
    with_env(
      "CENTAUR_CONSOLE_CENTAUR_DATABASE_URL" => nil,
      "CENTAUR_DATABASE_URL" => nil,
      "CENTAUR_CONSOLE_CENTAUR_DATABASE_NAME" => nil,
      "CENTAUR_DATABASE_NAME" => "ai_v2"
    ) do
      primary = {
        "adapter" => "postgresql",
        "encoding" => "unicode",
        "pool" => 5,
        "url" => "postgres://console_user:secret@db-host:6543/iron_control_development"
      }

      stub_primary(primary) do
        config = build_config

        assert_equal "ai_v2", config[:database]
        assert_not config.key?(:url), "resolved config should not carry a :url key"
        assert_equal "db-host", config[:host]
        assert_equal 6543, config[:port]
        assert_equal "console_user", config[:username]
      end
    end
  end

  test "fallback keeps ai_v2 for a host/port style primary config without a url" do
    with_env(
      "CENTAUR_CONSOLE_CENTAUR_DATABASE_URL" => nil,
      "CENTAUR_DATABASE_URL" => nil,
      "CENTAUR_CONSOLE_CENTAUR_DATABASE_NAME" => nil,
      "CENTAUR_DATABASE_NAME" => "ai_v2"
    ) do
      primary = {
        "adapter" => "postgresql",
        "host" => "localhost",
        "port" => 5432,
        "username" => "postgres",
        "database" => "iron_control_development"
      }

      stub_primary(primary) do
        config = build_config

        assert_equal "ai_v2", config[:database]
        assert_not config.key?(:url)
        assert_equal "localhost", config[:host]
      end
    end
  end

  test "explicit session url is used verbatim without touching the primary config" do
    with_env(
      "CENTAUR_CONSOLE_CENTAUR_DATABASE_URL" => "postgres://u:p@remote:5432/ai_v2",
      "CENTAUR_DATABASE_URL" => nil
    ) do
      config = build_config

      assert_equal "postgres://u:p@remote:5432/ai_v2", config[:url]
      assert_equal "postgresql", config[:adapter]
      assert_nil config[:database]
    end
  end
end
