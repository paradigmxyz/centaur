require_relative "boot"

require "rails/all"

# Require the gems listed in Gemfile, including any gems
# you've limited to :test, :development, or :production.
Bundler.require(*Rails.groups)

require_relative "../lib/json_log_formatter"
require_relative "../lib/secure_session_cookie"

module IronControl
  class Application < Rails::Application
    # Initialize configuration defaults for originally generated Rails version.
    config.load_defaults 8.1

    # Please, add to the `ignore` list any other `lib` subdirectories that do
    # not contain `.rb` files, or that should not be reloaded or eager loaded.
    # Common ones are `templates`, `generators`, or `middleware`, for example.
    config.autoload_lib(ignore: %w[assets tasks json_log_formatter.rb console_env.rb console_log_filter.rb secure_session_cookie.rb])

    # Configuration for the application, engines, and railties goes here.
    #
    # These settings can be overridden in specific environments using the files
    # in config/environments, which are processed later.
    #
    # config.time_zone = "Central Time (US & Canada)"
    # config.eager_load_paths << Rails.root.join("extras")

    # Mark the session cookie Secure on HTTPS requests, and always when the
    # public URL is https (see lib/secure_session_cookie.rb).
    config.middleware.insert_after ActionDispatch::Session::CookieStore, SecureSessionCookie,
                                   always: SecureSessionCookie.https_public_url?(ConsoleEnv["PUBLIC_URL"])
  end
end
