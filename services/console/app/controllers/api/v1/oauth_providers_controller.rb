module Api
  module V1
    # GET /api/v1/oauth_providers
    #
    # The supported OAuth app providers, in registry order (the first is the
    # form default), with the provider_config keys the custom provider takes.
    class OauthProvidersController < Api::BaseController
      credential_policy :session

      def index
        render json: { data: Oauth::Providers.registry.values.map { |provider| provider_payload(provider) } }
      end

      private

      def provider_payload(provider)
        custom = provider.key == Oauth::Providers::Custom::KEY
        {
          key: provider.key,
          display_name: provider.display_name,
          presettable: OauthPresetSetup.supported?(provider),
          config_keys: custom ? Oauth::Providers::Custom::CONFIG_KEYS : [],
          optional_config_keys: custom ? Oauth::Providers::Custom::OPTIONAL_CONFIG_KEYS : []
        }
      end
    end
  end
end
