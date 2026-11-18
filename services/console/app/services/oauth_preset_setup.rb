# Sets up a preset from the Integrations page: finds the preset's OAuth app by
# slug or, when it does not exist yet, registers a client with the provider
# (RFC 7591 dynamic registration) and creates the app. The caller then sends
# the admin through the app's consent flow.
#
# A preset is either a provider strategy with fixed endpoints (Mercator) or a
# standalone Oauth::Presets entry that configures a generic provider from what
# the admin enters (Runlayer: a tenant MCP URL; see #setup_inputs).
#
# Admin-only and CSRF-protected at the controller: it makes an outbound
# registration request and creates operator config.
class OauthPresetSetup
  # The preset's slug is taken by an app that does not match the preset
  # (another provider, the wrong shared/owner-only mode, or disabled).
  class Conflict < StandardError; end
  # The admin's input (e.g. the tenant URL) is missing or invalid.
  class InvalidInput < StandardError; end

  # Whether a provider strategy is itself a preset.
  def self.supported?(provider)
    provider.respond_to?(:preset) && provider.respond_to?(:registration_endpoint)
  end

  # Presets whose app does not exist yet. A provider-backed preset is also
  # hidden once any app uses its provider.
  def self.available_presets
    slugs = OauthApp.pluck(:slug)
    providers = OauthApp.distinct.pluck(:provider)
    Oauth::Presets.all.reject do |preset|
      slugs.include?(preset.preset.fetch(:slug)) || (supported?(preset) && providers.include?(preset.key))
    end
  end

  # +redirect_uri_for+ maps a slug to the callback URI registered with the
  # provider; +registration_client+ responds to #register; +input+ holds the
  # values a standalone preset asks for.
  def initialize(preset:, user:, redirect_uri_for:, registration_client:, input: {})
    @preset = preset
    @user = user
    @redirect_uri_for = redirect_uri_for
    @registration_client = registration_client
    @input = input
  end

  # Returns the preset's app. Raises Conflict, InvalidInput,
  # Broker::ExchangeError (the registration failed), or
  # ActiveRecord::RecordInvalid.
  def call
    attributes = @preset.preset
    app = OauthApp.find_by(slug: attributes.fetch(:slug)) || register_and_create(attributes)
    unless app.provider == provider_key(attributes) && app.shared? == attributes.fetch(:shared) &&
        app.owner_only? == attributes.fetch(:owner_only, false) && app.enabled?
      raise Conflict
    end

    app
  end

  private

  def provider_key(attributes) = attributes.fetch(:provider, @preset.key)

  def setup
    return { provider_config: {}, registration_endpoint: @preset.registration_endpoint } unless @preset.respond_to?(:setup_from)

    @preset.setup_from(@input) || raise(InvalidInput)
  end

  def register_and_create(attributes)
    slug = attributes.fetch(:slug)
    config = setup
    registration = @registration_client.register(
      endpoint: config.fetch(:registration_endpoint),
      redirect_uri: @redirect_uri_for.call(slug),
      # Registration scopes are space-separated (RFC 7591) whatever separator
      # the provider's consent request uses.
      scope: attributes.fetch(:allowed_scopes).join(" "),
      auth_method: @preset.respond_to?(:token_endpoint_auth_method) ? @preset.token_endpoint_auth_method : "client_secret_post"
    )
    begin
      OauthApp.create!(attributes.merge(registration).merge(provider: provider_key(attributes),
        provider_config: config.fetch(:provider_config), enabled: true, created_by: @user))
    rescue ActiveRecord::RecordNotUnique
      OauthApp.find_by!(slug: slug)
    rescue ActiveRecord::RecordInvalid => error
      # The uniqueness validator can observe a concurrent winner before INSERT.
      raise unless error.record.errors.of_kind?(:slug, :taken)

      OauthApp.find_by!(slug: slug)
    end
  end
end
