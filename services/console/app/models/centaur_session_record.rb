class CentaurSessionRecord < ActiveRecord::Base
  self.abstract_class = true

  DEFAULT_DATABASE_NAME = "ai_v2".freeze

  class << self
    private

    def session_database_configuration
      explicit_url =
        ConsoleEnv["CENTAUR_DATABASE_URL"].presence || ENV["CENTAUR_DATABASE_URL"].presence
      if explicit_url
        return {
          adapter: "postgresql",
          encoding: "unicode",
          pool: ENV.fetch("RAILS_MAX_THREADS", 5),
          url: explicit_url
        }
      end

      config = primary_database_configuration.deep_symbolize_keys

      # When the primary config carries a :url (the common single-URL dev setup
      # where database.yml's default block sets url: <DATABASE_URL>), Rails'
      # UrlConfig merges the URL-derived keys OVER sibling hash keys. That means
      # a database path inside the primary URL would override any :database we
      # set here, silently pointing the session models at the console's own DB.
      # Resolve the URL into discrete connection params and drop :url so the
      # ai_v2 database name below is authoritative.
      if config[:url].present?
        resolved = ActiveRecord::DatabaseConfigurations::ConnectionUrlResolver
          .new(config.delete(:url))
          .to_hash
          .symbolize_keys
        config = config.merge(resolved)
      else
        config.delete(:url)
      end

      config[:database] = session_database_name(config)
      config
    end

    def primary_database_configuration
      env_config = Rails.application.config.database_configuration.fetch(Rails.env)
      (env_config["primary"] || env_config).deep_dup
    end

    def session_database_name(config)
      ConsoleEnv["CENTAUR_DATABASE_NAME"].presence ||
        ENV["CENTAUR_DATABASE_NAME"].presence ||
        (Rails.env.test? ? config[:database] : DEFAULT_DATABASE_NAME)
    end
  end

  establish_connection session_database_configuration
end
