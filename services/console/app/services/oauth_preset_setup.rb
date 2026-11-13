# Sets up a provider preset from the Integrations page: finds the preset's
# OAuth app by slug or, when it does not exist yet, registers a client with
# the provider (RFC 7591 dynamic registration) and creates the app. The caller
# then sends the admin through the app's consent flow.
#
# Admin-only and CSRF-protected at the controller: it makes an outbound
# registration request and creates operator config.
class OauthPresetSetup
  # The preset's slug is taken by an app that does not match the preset
  # (another provider, the wrong shared mode, or disabled).
  class Conflict < StandardError; end

  def self.supported?(provider)
    provider.respond_to?(:preset) && provider.respond_to?(:registration_endpoint)
  end

  def self.available_providers
    configured = OauthApp.distinct.pluck(:provider)
    Oauth::Providers.registry.values.select { |provider| supported?(provider) && !configured.include?(provider.key) }
  end

  # +redirect_uri_for+ maps a slug to the callback URI registered with the
  # provider; +registration_client+ responds to #register.
  def initialize(provider:, user:, redirect_uri_for:, registration_client:)
    @provider = provider
    @user = user
    @redirect_uri_for = redirect_uri_for
    @registration_client = registration_client
  end

  # Returns the preset's app. Raises Conflict, Broker::ExchangeError (the
  # registration failed), or ActiveRecord::RecordInvalid.
  def call
    preset = @provider.preset
    app = OauthApp.find_by(slug: preset.fetch(:slug)) || register_and_create(preset)
    raise Conflict unless app.provider == @provider.key && app.shared? == preset.fetch(:shared) && app.enabled?

    app
  end

  private

  def register_and_create(preset)
    slug = preset.fetch(:slug)
    registration = @registration_client.register(
      endpoint: @provider.registration_endpoint,
      redirect_uri: @redirect_uri_for.call(slug),
      # Registration scopes are space-separated (RFC 7591) whatever separator
      # the provider's consent request uses.
      scope: preset.fetch(:allowed_scopes).join(" "),
      auth_method: @provider.respond_to?(:token_endpoint_auth_method) ? @provider.token_endpoint_auth_method : "client_secret_post"
    )
    begin
      OauthApp.create!(preset.merge(registration).merge(provider: @provider.key, enabled: true, created_by: @user))
    rescue ActiveRecord::RecordNotUnique
      OauthApp.find_by!(slug: slug)
    rescue ActiveRecord::RecordInvalid => error
      # The uniqueness validator can observe a concurrent winner before INSERT.
      raise unless error.record.errors.of_kind?(:slug, :taken)

      OauthApp.find_by!(slug: slug)
    end
  end
end
