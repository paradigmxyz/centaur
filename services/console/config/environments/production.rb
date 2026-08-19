require "active_support/core_ext/integer/time"
require "uri"

Rails.application.configure do
  # Settings specified here will take precedence over those in config/application.rb.

  # Code is not reloaded between requests.
  config.enable_reloading = false

  # Eager load code on boot for better performance and memory savings (ignored by Rake tasks).
  config.eager_load = true

  # Full error reports are disabled.
  config.consider_all_requests_local = false

  # Turn on fragment caching in view templates.
  config.action_controller.perform_caching = true

  # Cache assets for far-future expiry since they are all digest stamped.
  config.public_file_server.headers = { "cache-control" => "public, max-age=#{1.year.to_i}" }

  # Enable serving of images, stylesheets, and JavaScripts from an asset server.
  # config.asset_host = "http://assets.example.com"

  # Store uploaded files on the local file system (see config/storage.yml for options).
  config.active_storage.service = :local

  # Do not assume or force SSL here: in-cluster callers reach the service over
  # plain HTTP. Public TLS enforcement belongs at the ingress/proxy layer.
  # config.assume_ssl = true
  # config.force_ssl = true

  # Log to STDOUT as single-line JSON with the current request id as a default log tag.
  config.log_tags = [ :request_id ]
  config.logger   = ActiveSupport::TaggedLogging.logger(STDOUT)
  config.logger.formatter = JsonLogFormatter.new

  # Change to "debug" to log everything (including potentially personally-identifiable information!).
  config.log_level = ENV.fetch("RAILS_LOG_LEVEL", "info")

  # Skip ANSI color codes; they are noise inside JSON log entries.
  config.colorize_logging = false

  # Collapse the default multi-line request logs into a single JSON event per
  # request. The Raw formatter emits a hash that JsonLogFormatter merges into
  # the JSON log entry.
  config.lograge.enabled = true
  config.lograge.base_controller_class = %w[ActionController::Base ActionController::API]
  config.lograge.formatter = Lograge::Formatters::Raw.new
  config.lograge.custom_payload do |controller|
    { request_id: controller.request.request_id }
  end

  # Prevent health checks from clogging up the logs.
  config.silence_healthcheck_path = "/up"

  # Don't log any deprecations.
  config.active_support.report_deprecations = false

  # Replace the default in-process memory cache store with a durable alternative.
  config.cache_store = :solid_cache_store

  # Replace the default in-process and non-durable queuing backend for Active Job.
  config.active_job.queue_adapter = :solid_queue
  config.solid_queue.connects_to = { database: { writing: :queue } }

  # Ignore bad email addresses and do not raise email delivery errors.
  # Set this to true and configure the email server for immediate delivery to raise delivery errors.
  # config.action_mailer.raise_delivery_errors = false

  # Set host to be used by links generated in mailer templates.
  config.action_mailer.default_url_options = { host: "example.com" }

  # Specify outgoing SMTP server. Remember to add smtp/* credentials via bin/rails credentials:edit.
  # config.action_mailer.smtp_settings = {
  #   user_name: Rails.application.credentials.dig(:smtp, :user_name),
  #   password: Rails.application.credentials.dig(:smtp, :password),
  #   address: "smtp.example.com",
  #   port: 587,
  #   authentication: :plain
  # }

  # Enable locale fallbacks for I18n (makes lookups for any locale fall back to
  # the I18n.default_locale when a translation cannot be found).
  config.i18n.fallbacks = true

  # Do not dump schema after migrations.
  config.active_record.dump_schema_after_migration = false

  # Only use :id for inspections in production.
  config.active_record.attributes_for_inspect = [ :id ]

  # Enable DNS rebinding protection and other `Host` header attacks. Public
  # deployments should set CENTAUR_CONSOLE_PUBLIC_URL; add any extra internal
  # health-check or ingress hosts with CENTAUR_CONSOLE_ALLOWED_HOSTS.
  public_url = ConsoleEnv["PUBLIC_URL"].presence
  internal_url = ConsoleEnv["URL"].presence
  allowed_hosts = ConsoleEnv["ALLOWED_HOSTS"].to_s.split(/[,\s]+/).map(&:strip).reject(&:blank?)
  [ public_url, internal_url ].compact.each do |url|
    host = URI.parse(url).host
    allowed_hosts << host if host.present?
  end
  config.hosts.concat(allowed_hosts.uniq) if allowed_hosts.any?

  # Skip DNS rebinding protection for the default health check endpoint.
  config.host_authorization = { exclude: ->(request) { request.path == "/up" } }
end
