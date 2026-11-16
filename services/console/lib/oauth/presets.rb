module Oauth
  # Integrations presets an admin uses to register an OAuth app in one step.
  #
  # A provider-backed preset is a provider strategy with fixed endpoints that
  # defines #preset and #registration_endpoint (Mercator). A standalone preset
  # here configures a generic provider, such as custom, from what the admin
  # enters (#setup_inputs, #setup_from). Presets add no runtime behavior: once
  # the app exists it is an ordinary OauthApp. See OauthPresetSetup.
  module Presets
    def self.registry
      @registry ||= { Runlayer::KEY => Runlayer.new }.freeze
    end

    # Every preset, standalone and provider-backed, by key.
    def self.all
      providers = Oauth::Providers.registry.values.select { |provider| OauthPresetSetup.supported?(provider) }
      (registry.values + providers).sort_by(&:key)
    end

    def self.fetch(key)
      registry[key] || Oauth::Providers.fetch(key).then { |provider| provider if OauthPresetSetup.supported?(provider) }
    end
  end
end
