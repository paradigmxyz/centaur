# frozen_string_literal: true

if ENV["SENTRY_DSN"].present?
  Sentry.init do |config|
    config.breadcrumbs_logger = [ :active_support_logger ]
    config.dsn = ENV.fetch("SENTRY_DSN")
  end
end
