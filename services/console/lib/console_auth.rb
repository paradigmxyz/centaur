# Configuration for console SSO login. Unlike OauthApp (a DB-managed integration
# the broker mints credentials for), the login client is infrastructure: its
# client_id/client_secret and the bootstrap-admin allowlist come from the
# environment (or Rails credentials as a fallback), not a table.
#
# Per provider, looks up:
#   CENTAUR_CONSOLE_<PROVIDER>_CLIENT_ID / _CLIENT_SECRET (ENV)
#   credentials.console_auth.<provider>.client_id/secret  (fallback)
# A provider is offered on the login page only when both are present.
#
# SSO email domains are optional. When configured, every SSO login must use an
# email address under one of these domains:
#   CENTAUR_CONSOLE_SSO_EMAIL_DOMAINS="acme.com example.org"
#
# Password login is a break-glass fallback and can be disabled for public
# deployments:
#   CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED=false
#
# Bootstrap admins are matched by email and become active + admin on first login
# (the first admin needs no existing approver):
#   CENTAUR_CONSOLE_BOOTSTRAP_ADMINS="me@acme.com, you@acme.com"   (ENV)
#   credentials.console_auth.bootstrap_admins                   (fallback: string or array)
module ConsoleAuth
  # The providers a Login::Providers strategy exists for. A provider must also be
  # `configured?` to actually appear on the login page.
  SUPPORTED = %w[google slack].freeze

  module_function

  # Configured + supported provider keys, for the login page buttons.
  def providers
    SUPPORTED.select { |p| configured?(p) }
  end

  def configured?(provider)
    SUPPORTED.include?(provider.to_s) && client_id(provider).present? && client_secret(provider).present?
  end

  def client_id(provider) = setting(provider, "client_id")
  def client_secret(provider) = setting(provider, "client_secret")

  def password_login_enabled?
    raw = ConsoleEnv["PASSWORD_LOGIN_ENABLED"]
    raw.nil? ? true : boolean_setting(raw)
  end

  def sso_email_allowed?(email)
    domains = sso_email_domains
    return true if domains.empty?

    domain = email.to_s.strip.downcase.split("@", 2).last
    domains.include?(domain)
  end

  def sso_email_domains
    raw = ConsoleEnv["SSO_EMAIL_DOMAINS"].presence
    raw.to_s.split(/[,\s]+/).map { |domain| domain.strip.downcase }.reject(&:empty?).uniq
  end

  def bootstrap_admin?(email)
    normalized = email.to_s.strip.downcase
    return false if normalized.empty?
    bootstrap_admins.include?(normalized)
  end

  def bootstrap_admins
    raw = ConsoleEnv["BOOTSTRAP_ADMINS"].presence || credentials_dig(:bootstrap_admins)
    list = raw.is_a?(Array) ? raw : raw.to_s.split(/[,\s]+/)
    list.map { |e| e.to_s.strip.downcase }.reject(&:empty?).uniq
  end

  # ENV first (CENTAUR_CONSOLE_GOOGLE_CLIENT_ID), then credentials
  # (console_auth.google.client_id).
  def setting(provider, field)
    env = ConsoleEnv["#{provider.to_s.upcase}_#{field.upcase}"].presence
    return env if env
    credentials_dig(provider.to_sym, field.to_sym)
  end

  def credentials_dig(*path)
    Rails.application.credentials.dig(:console_auth, *path)
  end

  def boolean_setting(value)
    ActiveModel::Type::Boolean.new.cast(value)
  end
end
